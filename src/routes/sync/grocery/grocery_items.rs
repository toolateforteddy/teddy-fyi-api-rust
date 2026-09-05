use crate::routes::sync::deletes::soft_delete_version;
use crate::routes::sync::types::*;
use crate::routes::sync::versioning::{advance_version, seed_version};
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

#[allow(clippy::too_many_arguments)]
pub async fn process_grocery_changes(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    client_id: &str,
    server_timestamp: DateTime<Utc>,
    changes: &[GroceryChangeDelta],
    success_ids: &mut Vec<String>,
    upload_status: &mut Vec<SuccessResult>,
    remote_changes: &mut Vec<GroceryChangeDelta>,
) -> Result<(), AppError> {
    let change_ids: Vec<String> = changes.iter().map(|c| c.id.clone()).collect();
    let existing_records = sqlx::query!(
        r#"SELECT id, name, quantity, "isBought" as is_bought, "createdAt" as created_at, position, "categoryId" as category_id, "timesBought" as times_bought, "userId" as user_id, "isActive" as is_active, "listId" as list_id, unit, notes, version, is_deleted, sync_state FROM grocery_items WHERE id = ANY($1)"#,
        &change_ids
    )
    .fetch_all(&mut **tx)
    .await?;

    let existing_map: std::collections::HashMap<String, _> = existing_records
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect();

    let mut list_ids = std::collections::HashSet::new();
    for change in changes {
        if let Some(ref data) = change.data {
            if let Ok(item) = serde_json::from_value::<GroceryItemData>(data.clone()) {
                if let Some(ref list_id) = item.list_id {
                    list_ids.insert(list_id.clone());
                }
            }
        }
        if let Some(row) = existing_map.get(&change.id) {
            if let Some(ref list_id) = row.list_id {
                list_ids.insert(list_id.clone());
            }
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

    let existing_store_infos = sqlx::query!(
        r#"SELECT "groceryItemId" as grocery_item_id, "storeId" as store_id FROM grocery_item_store_info WHERE "groceryItemId" = ANY($1)"#,
        &change_ids
    )
    .fetch_all(&mut **tx)
    .await?;

    let mut existing_store_info_set = std::collections::HashSet::new();
    for row in existing_store_infos {
        existing_store_info_set.insert((row.grocery_item_id, row.store_id));
    }

    // Auto-populated store mappings, resolved for the whole batch in one query.
    // This used to run a three-table join once per grocery change, so a payload at
    // `SYNC_MAX_ITEMS_PER_COLLECTION` meant up to 10,000 joins on the single pooled
    // connection this transaction holds. Only rows the server has never seen need the
    // mapping (see the call site below), so only their names are looked up.
    let mut mapping_lookup_names: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for change in changes {
        if existing_map.contains_key(&change.id) {
            continue;
        }
        if !matches!(
            change.operation_type,
            OperationType::Insert | OperationType::Update
        ) {
            continue;
        }
        if let Some(ref data) = change.data {
            if let Ok(item) = serde_json::from_value::<GroceryItemData>(data.clone()) {
                mapping_lookup_names.insert(item.name.to_lowercase());
            }
        }
    }

    // The lookup key is the Rust-lowercased name, and the query filters on exactly those
    // strings, so every key the query returns is one we can look up again by that name.
    let mut mappings_by_name: std::collections::HashMap<
        String,
        Vec<(String, Option<f64>, bool)>,
    > = std::collections::HashMap::new();
    if !mapping_lookup_names.is_empty() {
        let mapping_names: Vec<String> = mapping_lookup_names.into_iter().collect();
        let mapping_rows = sqlx::query!(
            r#"
            SELECT DISTINCT LOWER(gi.name) as "name_key!", gsi."storeId" as store_id, gsi.price, gsi."isAvailable" as is_available
            FROM grocery_item_store_info gsi
            JOIN grocery_items gi ON gsi."groceryItemId" = gi.id
            JOIN grocery_list_members glm ON gi."listId" = glm."listId"
            WHERE LOWER(gi.name) = ANY($1)
              AND glm."userId" = $2
              AND gi.is_deleted = FALSE
              AND gsi.is_deleted = FALSE
            "#,
            &mapping_names,
            user_id
        )
        .fetch_all(&mut **tx)
        .await?;

        for row in mapping_rows {
            mappings_by_name
                .entry(row.name_key)
                .or_default()
                .push((row.store_id, row.price, row.is_available));
        }
    }

    // Backfill rows are collected here and written with one multi-row insert after the
    // loop; nothing between here and there reads `grocery_item_store_info`.
    let mut pending_store_info: Vec<(String, String, Option<f64>, bool)> = Vec::new();

    for change in changes {
        let string_id = change.id.clone();
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!("Processing grocery item {}", change.id);

                let is_need_update = matches!(change.operation_type, OperationType::Update)
                    && (change.data.is_none() || change.data.as_ref().map(|v| v.is_null()).unwrap_or(false));

                if is_need_update {
                    if let Some(row) = existing_map.get(&change.id) {
                        let mut authorized = false;
                        if let Some(ref list_id) = row.list_id {
                            if member_lists_set.contains(list_id) {
                                authorized = true;
                            }
                        } else {
                            if row.user_id.as_deref() == Some(user_id) {
                                authorized = true;
                            }
                        }
                        if !authorized {
                            return Err(AppError::Forbidden(format!("User is not authorized to update grocery item {}", change.id)));
                        }

                        let item_data = GroceryItemData {
                            id: change.id.clone(),
                            name: row.name.clone(),
                            quantity: row.quantity.clone(),
                            is_bought: row.is_bought,
                            created_at: row.created_at,
                            position: row.position,
                            category_id: row.category_id.clone(),
                            times_bought: row.times_bought,
                            user_id: row.user_id.clone(),
                            is_active: row.is_active,
                            list_id: row.list_id.clone(),
                            unit: row.unit.clone(),
                            notes: row.notes.clone(),
                            version: row.version,
                            is_deleted: row.is_deleted,
                            sync_state: row.sync_state.clone(),
                        };
                        let data_val = serde_json::to_value(&item_data)?;
                        remote_changes.push(GroceryChangeDelta {
                            id: change.id.clone(),
                            operation_type: OperationType::Update,
                            version: row.version,
                            data: Some(data_val),
                        });
                        success_ids.push(change.id.clone());
                    }
                    continue;
                }

                if let Some(ref data) = change.data {
                    match serde_json::from_value::<GroceryItemData>(data.clone()) {
                        Ok(item) => {
                            // Verify permission: User must belong to the list specified by list_id (if any)
                            if let Some(ref list_id) = item.list_id {
                                if !member_lists_set.contains(list_id) {
                                    return Err(AppError::Forbidden(format!(
                                        "User is not a member of list {}",
                                        list_id
                                    )));
                                }
                            }

                            let record = existing_map.get(&change.id);

                            if record.is_some() && matches!(change.operation_type, OperationType::Update) {
                                // For Update, verify existing item's list membership too
                                if let Some(row) = record {
                                    if let Some(ref list_id) = row.list_id {
                                        if !member_lists_set.contains(list_id) {
                                            return Err(AppError::Forbidden(format!(
                                                "User is not authorized to update grocery item in list {}",
                                                list_id
                                            )));
                                        }
                                    } else {
                                        if row.user_id.as_deref() != Some(user_id) {
                                            return Err(AppError::Forbidden(format!("User is not authorized to update grocery item {}", change.id)));
                                        }
                                    }
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
                                        "Conflicting write for grocery {} (client version {}, server version {}); accepting it as the later arrival",
                                        change.id, change.version, row.version
                                    );
                                }
                                advance_version("Grocery", &change.id, row.version)?
                            } else {
                                seed_version("Grocery", &change.id, item.version)?
                            };

                            sqlx::query!(
                                r#"
                                INSERT INTO grocery_items (
                                    id, name, quantity, "isBought", "createdAt", position, "categoryId",
                                    "timesBought", "userId", "isActive", "listId", unit, notes, version,
                                    is_deleted, sync_state, updated_at, updated_by_client
                                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)
                                ON CONFLICT (id) DO UPDATE SET
                                    name = EXCLUDED.name,
                                    quantity = EXCLUDED.quantity,
                                    "isBought" = EXCLUDED."isBought",
                                    "createdAt" = EXCLUDED."createdAt",
                                    position = EXCLUDED.position,
                                    "categoryId" = EXCLUDED."categoryId",
                                    "timesBought" = EXCLUDED."timesBought",
                                    "userId" = EXCLUDED."userId",
                                    "isActive" = EXCLUDED."isActive",
                                    "listId" = EXCLUDED."listId",
                                    unit = EXCLUDED.unit,
                                    notes = EXCLUDED.notes,
                                    version = EXCLUDED.version,
                                    is_deleted = EXCLUDED.is_deleted,
                                    sync_state = EXCLUDED.sync_state,
                                    updated_at = EXCLUDED.updated_at,
                                    updated_by_client = EXCLUDED.updated_by_client
                                "#,
                                item.id,
                                item.name,
                                item.quantity,
                                item.is_bought,
                                item.created_at,
                                item.position,
                                item.category_id,
                                item.times_bought,
                                user_id, // override with authenticated user_id
                                item.is_active,
                                item.list_id,
                                item.unit,
                                item.notes,
                                next_version,
                                item.is_deleted,
                                "SYNCED",
                                server_timestamp,
                                client_id
                            )
                            .execute(&mut **tx)
                            .await?;

                            // Auto-populate store mapping, but only the first time the
                            // server sees this row: the backfill is a convenience for a
                            // brand new item, and a row already in `existing_map` went
                            // through it on the sync that created it.
                            if record.is_none() {
                                if let Some(mappings) = mappings_by_name.get(&item.name.to_lowercase()) {
                                    for (store_id, price, is_available) in mappings {
                                        // `existing_store_info_set` also absorbs what this
                                        // batch has already queued, so two changes naming the
                                        // same (item, store) pair no longer both try to insert.
                                        if existing_store_info_set
                                            .insert((item.id.clone(), store_id.clone()))
                                        {
                                            pending_store_info.push((
                                                item.id.clone(),
                                                store_id.clone(),
                                                *price,
                                                *is_available,
                                            ));
                                        }
                                    }
                                }
                            }

                            upload_status.push(SuccessResult {
                                id: string_id.clone(),
                                version: next_version,
                                sync_state: "SYNCED".to_string(),
                            });
                            success_ids.push(string_id);
                        }
                        Err(err) => {
                            tracing::error!(
                                "Failed to deserialize GroceryItemData for grocery {}: {:?}. Data: {:?}",
                                change.id,
                                err,
                                data
                            );
                            return Err(AppError::Serialization(err));
                        }
                    }
                } else if matches!(change.operation_type, OperationType::Update) {
                    let record = existing_map.get(&change.id);

                    if let Some(row) = record {
                        if let Some(ref list_id) = row.list_id {
                            if !member_lists_set.contains(list_id) {
                                return Err(AppError::Forbidden(format!(
                                    "User is not authorized to update grocery item in list {}",
                                    list_id
                                )));
                            }
                        } else {
                            if row.user_id.as_deref() != Some(user_id) {
                                return Err(AppError::Forbidden(format!("User is not authorized to update grocery item {}", change.id)));
                            }
                        }

                        // Bounded like every other version bump here; see `crate::routes::sync::versioning`.
                        let next_version = advance_version("Grocery", &change.id, row.version)?;
                        sqlx::query!(
                            "UPDATE grocery_items SET version = $1, updated_at = $2, updated_by_client = $3, sync_state = 'SYNCED' WHERE id = $4",
                            next_version,
                            server_timestamp,
                            client_id,
                            change.id
                        )
                        .execute(&mut **tx)
                        .await?;

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
                let record = existing_map.get(&change.id);

                if let Some(row) = record {
                    if row.is_deleted {
                        upload_status.push(SuccessResult {
                            id: string_id.clone(),
                            version: row.version,
                            sync_state: "SYNCED".to_string(),
                        });
                        success_ids.push(string_id);
                        continue;
                    }

                    if let Some(ref list_id) = row.list_id {
                        if !member_lists_set.contains(list_id) {
                            return Err(AppError::Forbidden(format!(
                                "User is not authorized to delete grocery item in list {}",
                                list_id
                            )));
                        }
                    } else if row.user_id.as_deref() != Some(user_id) {
                        return Err(AppError::Forbidden(format!("User is not authorized to delete grocery item {}", change.id)));
                    }
                }

                // Outside the guard above, which only decides authorization: a delete for
                // a row the server never had is acknowledged rather than left pending, so
                // the client can stop resending it. See `crate::routes::sync::deletes`.
                let version = soft_delete_version!(
                    tx,
                    "grocery item",
                    &change.id,
                    "UPDATE grocery_items SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2 WHERE id = $3 RETURNING version",
                    server_timestamp,
                    client_id,
                    change.id,
                );

                upload_status.push(SuccessResult {
                    id: string_id.clone(),
                    version,
                    sync_state: "SYNCED".to_string(),
                });
                success_ids.push(string_id);
            }
        }
    }

    if !pending_store_info.is_empty() {
        let item_ids: Vec<String> = pending_store_info.iter().map(|m| m.0.clone()).collect();
        let store_ids: Vec<String> = pending_store_info.iter().map(|m| m.1.clone()).collect();
        let prices: Vec<Option<f64>> = pending_store_info.iter().map(|m| m.2).collect();
        let availabilities: Vec<bool> = pending_store_info.iter().map(|m| m.3).collect();

        sqlx::query!(
            r#"
            INSERT INTO grocery_item_store_info (
                "groceryItemId", "storeId", price, "isAvailable", "userId", version, is_deleted, sync_state, updated_at, updated_by_client
            )
            SELECT m.item_id, m.store_id, m.price, m.is_available, $5, 1, FALSE, 'SYNCED', $6, NULL::text
            FROM UNNEST($1::text[], $2::text[], $3::double precision[], $4::bool[])
                AS m(item_id, store_id, price, is_available)
            ON CONFLICT ("groceryItemId", "storeId") DO NOTHING
            "#,
            &item_ids,
            &store_ids,
            &prices as &[Option<f64>],
            &availabilities,
            user_id,
            server_timestamp
        )
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}
