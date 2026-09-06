use crate::routes::sync::batching::RunTracker;
use crate::routes::sync::deletes::ack_unsynced_delete;
use crate::routes::sync::types::*;
use crate::routes::sync::versioning::{advance_version, seed_version};
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};
use std::collections::HashMap;

/// The kinds of write this processor issues. A run may only contain one of them; see
/// `crate::routes::sync::batching`.
#[derive(PartialEq, Eq)]
enum WriteKind {
    Upsert,
    VersionBump,
    Delete,
}

/// The row this table is keyed by, as one string, so a run can tell whether it already
/// holds it. The key is a pair here rather than an id, and the client-supplied `id` field
/// is optional and not the primary key, so it cannot stand in for one.
fn row_key(grocery_item_id: &str, store_id: &str) -> String {
    format!("{}\u{1f}{}", grocery_item_id, store_id)
}

/// The column vectors for the run currently being accumulated: one `Vec` per column,
/// because that is the shape `UNNEST($1::text[], $2::int4[], ...)` zips back into rows.
#[derive(Default)]
struct Pending {
    up_item: Vec<String>,
    up_store: Vec<String>,
    up_price: Vec<Option<f64>>,
    up_available: Vec<bool>,
    up_version: Vec<i32>,
    up_is_deleted: Vec<bool>,

    bump_item: Vec<String>,
    bump_store: Vec<String>,
    bump_version: Vec<i32>,

    del_item: Vec<String>,
    del_store: Vec<String>,
    /// Where in `upload_status` each buffered delete's placeholder sits; the version comes
    /// back from the statement, but the entry has to keep its place in the response.
    del_status_idx: Vec<usize>,
    /// The id each buffered delete is reported under, for the log line an unsynced delete
    /// still emits.
    del_report_id: Vec<String>,
}

impl Pending {
    async fn flush(
        &mut self,
        tx: &mut Transaction<'_, Postgres>,
        user_id: &str,
        client_id: &str,
        server_timestamp: DateTime<Utc>,
        upload_status: &mut [SuccessResult],
    ) -> Result<(), AppError> {
        if !self.up_item.is_empty() {
            sqlx::query!(
                r#"
                INSERT INTO grocery_item_store_info (
                    "groceryItemId", "storeId", price, "isAvailable", "userId", version, is_deleted, sync_state, updated_at, updated_by_client
                )
                SELECT v.item_id, v.store_id, v.price, v.is_available, $7, v.version, v.is_deleted, 'SYNCED', $8, $9
                FROM UNNEST($1::text[], $2::text[], $3::float8[], $4::bool[], $5::int4[], $6::bool[])
                    AS v(item_id, store_id, price, is_available, version, is_deleted)
                ON CONFLICT ("groceryItemId", "storeId") DO UPDATE SET
                    price = EXCLUDED.price,
                    "isAvailable" = EXCLUDED."isAvailable",
                    "userId" = EXCLUDED."userId",
                    version = EXCLUDED.version,
                    is_deleted = EXCLUDED.is_deleted,
                    sync_state = EXCLUDED.sync_state,
                    updated_at = EXCLUDED.updated_at,
                    updated_by_client = EXCLUDED.updated_by_client
                "#,
                &self.up_item,
                &self.up_store,
                &self.up_price as &[Option<f64>],
                &self.up_available,
                &self.up_version,
                &self.up_is_deleted,
                user_id,
                server_timestamp,
                client_id
            )
            .execute(&mut **tx)
            .await?;

            self.up_item.clear();
            self.up_store.clear();
            self.up_price.clear();
            self.up_available.clear();
            self.up_version.clear();
            self.up_is_deleted.clear();
        }

        if !self.bump_item.is_empty() {
            sqlx::query!(
                r#"
                UPDATE grocery_item_store_info SET
                    version = v.version,
                    updated_at = $4,
                    updated_by_client = $5,
                    sync_state = 'SYNCED'
                FROM UNNEST($1::text[], $2::text[], $3::int4[]) AS v(item_id, store_id, version)
                WHERE grocery_item_store_info."groceryItemId" = v.item_id
                  AND grocery_item_store_info."storeId" = v.store_id
                "#,
                &self.bump_item,
                &self.bump_store,
                &self.bump_version,
                server_timestamp,
                client_id
            )
            .execute(&mut **tx)
            .await?;

            self.bump_item.clear();
            self.bump_store.clear();
            self.bump_version.clear();
        }

        if !self.del_item.is_empty() {
            // `RETURNING` the key as well as the version, so the rows the server actually
            // had can be told apart from the ones it never saw; the latter are
            // acknowledged rather than failing the batch, exactly as the
            // per-item delete did. See `crate::routes::sync::deletes`.
            let updated = sqlx::query!(
                r#"
                UPDATE grocery_item_store_info SET
                    is_deleted = TRUE,
                    version = version + 1,
                    updated_at = $1,
                    updated_by_client = $2
                FROM UNNEST($3::text[], $4::text[]) AS v(item_id, store_id)
                WHERE grocery_item_store_info."groceryItemId" = v.item_id
                  AND grocery_item_store_info."storeId" = v.store_id
                RETURNING grocery_item_store_info."groceryItemId" as grocery_item_id, grocery_item_store_info."storeId" as store_id, grocery_item_store_info.version
                "#,
                server_timestamp,
                client_id,
                &self.del_item,
                &self.del_store
            )
            .fetch_all(&mut **tx)
            .await?;

            let deleted: HashMap<(String, String), i32> = updated
                .into_iter()
                .map(|r| ((r.grocery_item_id, r.store_id), r.version))
                .collect();

            for i in 0..self.del_item.len() {
                let key = (self.del_item[i].clone(), self.del_store[i].clone());
                let version = match deleted.get(&key) {
                    Some(version) => *version,
                    None => ack_unsynced_delete("grocery item store info", &self.del_report_id[i]),
                };
                upload_status[self.del_status_idx[i]].version = version;
            }

            self.del_item.clear();
            self.del_store.clear();
            self.del_status_idx.clear();
            self.del_report_id.clear();
        }

        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn process_grocery_item_store_info_changes(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    client_id: &str,
    server_timestamp: DateTime<Utc>,
    changes: &[GroceryItemStoreInfoChangeDelta],
    success_ids: &mut Vec<String>,
    upload_status: &mut Vec<SuccessResult>,
    remote_changes: &mut Vec<GroceryItemStoreInfoChangeDelta>,
) -> Result<(), AppError> {
    let parent_item_ids: Vec<String> = changes.iter().map(|c| c.grocery_item_id.clone()).collect();
    let parent_items = sqlx::query!(
        r#"SELECT id, "userId" as user_id, "listId" as list_id, is_deleted FROM grocery_items WHERE id = ANY($1)"#,
        &parent_item_ids
    )
    .fetch_all(&mut **tx)
    .await?;

    let parent_items_map: std::collections::HashMap<String, _> = parent_items
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect();

    let mut list_ids = std::collections::HashSet::new();
    for row in parent_items_map.values() {
        if let Some(ref list_id) = row.list_id {
            list_ids.insert(list_id.clone());
        }
    }
    let list_ids_vec: Vec<String> = list_ids.into_iter().collect();

    let membership_records = sqlx::query!(
        r#"SELECT "listId" as list_id FROM grocery_list_members WHERE "userId" = $1 AND "listId" = ANY($2) AND is_deleted = FALSE"#,
        user_id,
        &list_ids_vec
    )
    .fetch_all(&mut **tx)
    .await?;

    let member_lists_set: std::collections::HashSet<String> = membership_records
        .into_iter()
        .map(|r| r.list_id)
        .collect();

    let existing_infos = sqlx::query!(
        r#"SELECT "groceryItemId" as grocery_item_id, "storeId" as store_id, price, "isAvailable" as is_available, "userId" as user_id, version, is_deleted, sync_state FROM grocery_item_store_info WHERE "groceryItemId" = ANY($1)"#,
        &parent_item_ids
    )
    .fetch_all(&mut **tx)
    .await?;

    let mut existing_map = std::collections::HashMap::new();
    for row in existing_infos {
        existing_map.insert((row.grocery_item_id.clone(), row.store_id.clone()), row);
    }

    // Writes are buffered into runs of one kind and flushed as a single statement each.
    // Everything above a write -- authorization, version assignment, what goes into the
    // response and in which order -- is unchanged and still decided per item.
    let mut runs: RunTracker<WriteKind> = RunTracker::new();
    let mut pending = Pending::default();

    for change in changes {
        let string_id = if !change.id.is_empty() {
            change.id.clone()
        } else {
            format!("{}-{}", change.grocery_item_id, change.store_id)
        };
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!(
                    "Processing grocery item store info for grocery {}, store {}",
                    change.grocery_item_id,
                    change.store_id
                );

                let is_need_update = matches!(change.operation_type, OperationType::Update)
                    && (change.data.is_none() || change.data.as_ref().map(|v| v.is_null()).unwrap_or(false));

                if is_need_update {
                    let parent = parent_items_map.get(&change.grocery_item_id);
                    if let Some(parent) = parent {
                        let mut authorized = parent.user_id.as_deref() == Some(user_id);
                        if !authorized {
                            if let Some(ref list_id) = parent.list_id {
                                if member_lists_set.contains(list_id) {
                                    authorized = true;
                                }
                            }
                        }
                        if !authorized {
                            return Err(AppError::Forbidden(format!(
                                "User is not authorized to update store info for item {} store {}",
                                change.grocery_item_id, change.store_id
                            )));
                        }
                    } else {
                        return Err(AppError::Forbidden(format!("Parent grocery item not found: {}", change.grocery_item_id)));
                    }

                    let existing = existing_map.get(&(change.grocery_item_id.clone(), change.store_id.clone()));

                    if let Some(row) = existing {
                        let item_data = GroceryItemStoreInfoData {
                            id: string_id.clone(),
                            grocery_item_id: change.grocery_item_id.clone(),
                            store_id: change.store_id.clone(),
                            list_id: parent_items_map
                                .get(&change.grocery_item_id)
                                .and_then(|r| r.list_id.clone()),
                            price: row.price,
                            is_available: row.is_available,
                            user_id: row.user_id.clone(),
                            version: row.version,
                            is_deleted: row.is_deleted,
                            sync_state: row.sync_state.clone(),
                        };
                        let data_val = serde_json::to_value(&item_data)?;
                        remote_changes.push(GroceryItemStoreInfoChangeDelta {
                            id: string_id.clone(),
                            grocery_item_id: change.grocery_item_id.clone(),
                            store_id: change.store_id.clone(),
                            operation_type: OperationType::Update,
                            version: row.version,
                            data: Some(data_val),
                        });
                        success_ids.push(string_id);
                    }
                    continue;
                }

                if let Some(ref data) = change.data {
                    match serde_json::from_value::<GroceryItemStoreInfoData>(data.clone()) {
                        Ok(item) => {
                            let record = existing_map.get(&(item.grocery_item_id.clone(), item.store_id.clone()));

                            if record.is_some() {
                                let parent = parent_items_map.get(&item.grocery_item_id);
                                if let Some(parent) = parent {
                                    let mut authorized = parent.user_id.as_deref() == Some(user_id);
                                    if !authorized {
                                        if let Some(ref list_id) = parent.list_id {
                                            if member_lists_set.contains(list_id) {
                                                authorized = true;
                                            }
                                        }
                                    }
                                    if !authorized {
                                        return Err(AppError::Forbidden(format!(
                                            "User is not authorized to update store info for item {} store {}",
                                            item.grocery_item_id, item.store_id
                                        )));
                                    }
                                } else {
                                    return Err(AppError::Forbidden(format!("Parent grocery item not found: {}", item.grocery_item_id)));
                                }
                            }

                            // One policy for every synced row: the server's stored version is the only
                            // input to the next one, and a row the server has never seen takes a bounded
                            // seed. `max(row.version, item.version) + 1` let a single request carrying an
                            // enormous `version` move this row's counter there permanently -- for a shared
                            // list, for every member of it. See `crate::routes::sync::versioning`.
                            let next_version = if let Some(row) = record {
                                if matches!(change.operation_type, OperationType::Update) && change.version < row.version {
                                    tracing::warn!(
                                        "Conflicting write for store info {} (client version {}, server version {}); accepting it as the later arrival",
                                        change.id, change.version, row.version
                                    );
                                }
                                advance_version("Store info", &change.id, row.version)?
                            } else {
                                seed_version("Store info", &change.id, item.version)?
                            };

                            let key = row_key(&item.grocery_item_id, &item.store_id);
                            if runs.needs_flush(&WriteKind::Upsert, &key) {
                                pending
                                    .flush(tx, user_id, client_id, server_timestamp, upload_status)
                                    .await?;
                                runs.clear();
                            }
                            runs.record(WriteKind::Upsert, key);

                            // `"userId"` is still the authenticated user, not the one on
                            // the wire; it is the same for every row in the request, so it
                            // is a scalar in the batched statement rather than an array.
                            pending.up_item.push(item.grocery_item_id);
                            pending.up_store.push(item.store_id);
                            pending.up_price.push(item.price);
                            pending.up_available.push(item.is_available);
                            pending.up_version.push(next_version);
                            pending.up_is_deleted.push(item.is_deleted);

                            upload_status.push(SuccessResult {
                                id: string_id.clone(),
                                version: next_version,
                                sync_state: "SYNCED".to_string(),
                            });
                            success_ids.push(string_id);
                        }
                        Err(err) => {
                            tracing::error!(
                                "Failed to deserialize GroceryItemStoreInfoData for item {}-{}: {:?}. Data: {:?}",
                                change.grocery_item_id, change.store_id,
                                err,
                                data
                            );
                            return Err(crate::routes::sync::rejections::item_payload_rejected(
                                "grocery item store info",
                                &change.id.to_string(),
                                &err,
                            ));
                        }
                    }
                } else if matches!(change.operation_type, OperationType::Update) {
                    let parent = parent_items_map.get(&change.grocery_item_id);
                    if let Some(parent) = parent {
                        let mut authorized = parent.user_id.as_deref() == Some(user_id);
                        if !authorized {
                            if let Some(ref list_id) = parent.list_id {
                                if member_lists_set.contains(list_id) {
                                    authorized = true;
                                }
                            }
                        }
                        if !authorized {
                            return Err(AppError::Forbidden(format!(
                                "User is not authorized to update store info for item {} store {}",
                                change.grocery_item_id, change.store_id
                            )));
                        }
                    } else {
                        return Err(AppError::Forbidden(format!("Parent grocery item not found: {}", change.grocery_item_id)));
                    }

                    let record = existing_map.get(&(change.grocery_item_id.clone(), change.store_id.clone()));

                    if let Some(row) = record {
                        // Bounded like every other version bump here; see `crate::routes::sync::versioning`.
                        let next_version = advance_version("Store info", &change.grocery_item_id, row.version)?;

                        let key = row_key(&change.grocery_item_id, &change.store_id);
                        if runs.needs_flush(&WriteKind::VersionBump, &key) {
                            pending
                                .flush(tx, user_id, client_id, server_timestamp, upload_status)
                                .await?;
                            runs.clear();
                        }
                        runs.record(WriteKind::VersionBump, key);

                        pending.bump_item.push(change.grocery_item_id.clone());
                        pending.bump_store.push(change.store_id.clone());
                        pending.bump_version.push(next_version);

                        upload_status.push(SuccessResult {
                            id: string_id.clone(),
                            version: next_version,
                            sync_state: "SYNCED".to_string(),
                        });
                        success_ids.push(string_id);
                    }
                }
            }
            OperationType::Delete => {
                let existing_info = existing_map.get(&(change.grocery_item_id.clone(), change.store_id.clone()));

                if let Some(info) = existing_info {
                    if info.is_deleted {
                        upload_status.push(SuccessResult {
                            id: string_id.clone(),
                            version: info.version,
                            sync_state: "SYNCED".to_string(),
                        });
                        success_ids.push(string_id);
                        continue;
                    }
                }

                // A parent the server does not have is the unsynced-delete case one level
                // up -- the item was created and deleted before it ever reached us -- and it
                // used to be a 403 that failed the whole batch. Acknowledged like any other
                // now, and nothing is left unguarded by it: `"groceryItemId"` is a foreign
                // key onto `grocery_items` with `ON DELETE CASCADE`, so a store-info row
                // whose parent is absent cannot exist and the statement below matches
                // nothing. Only a parent that *is* here and is not the caller's is a 403.
                if let Some(parent) = parent_items_map.get(&change.grocery_item_id) {
                    let mut authorized = parent.is_deleted;
                    if !authorized {
                        authorized = parent.user_id.as_deref() == Some(user_id);
                        if !authorized {
                            if let Some(ref list_id) = parent.list_id {
                                if member_lists_set.contains(list_id) {
                                    authorized = true;
                                }
                            }
                        }
                    }
                    if !authorized {
                        return Err(AppError::Forbidden(format!(
                            "User is not authorized to delete store info for item {} store {}",
                            change.grocery_item_id, change.store_id
                        )));
                    }
                }

                let key = row_key(&change.grocery_item_id, &change.store_id);
                if runs.needs_flush(&WriteKind::Delete, &key) {
                    pending
                        .flush(tx, user_id, client_id, server_timestamp, upload_status)
                        .await?;
                    runs.clear();
                }
                runs.record(WriteKind::Delete, key);

                pending.del_item.push(change.grocery_item_id.clone());
                pending.del_store.push(change.store_id.clone());
                pending.del_status_idx.push(upload_status.len());
                pending.del_report_id.push(string_id.clone());

                upload_status.push(SuccessResult {
                    id: string_id.clone(),
                    // Patched by the flush that issues this delete, which is what learns
                    // the row's new version.
                    version: 0,
                    sync_state: "SYNCED".to_string(),
                });
                success_ids.push(string_id);
            }
        }
    }

    pending
        .flush(tx, user_id, client_id, server_timestamp, upload_status)
        .await?;

    Ok(())
}
