use crate::routes::sync::device::{ItemDeviceRule, resolve_item_device};
use crate::routes::sync::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// A config row already on the server that an incoming write should land on.
struct ExistingConfig {
    id: Uuid,
    version: i32,
    last_modified: i64,
}

/// Where an incoming config write should land.
///
/// The unique key is `(user_id, device_uuid, key)`, so a row can already exist either
/// under the id the client sent or under that key with a different id — two clients that
/// independently created the same key produce exactly that. Resolving both up front lets
/// the write reconcile the id instead of tripping the constraint.
struct ConfigTarget {
    existing: Option<ExistingConfig>,
    /// The id the row carries after the write. The client's id is adopted when it is free;
    /// otherwise the server's id stands, because changing it would collide with the row
    /// that already holds it.
    new_id: Uuid,
}

async fn resolve_config_target(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    device_uuid: Uuid,
    submitted_id: Uuid,
    key: &str,
) -> Result<ConfigTarget, AppError> {
    let by_key = sqlx::query!(
        "SELECT id, version, last_modified FROM configs WHERE user_id = $1 AND device_uuid = $2 AND key = $3",
        user_id,
        device_uuid,
        key
    )
    .fetch_optional(&mut **tx)
    .await?;

    // Writes are strictly device-scoped, cloud dashboard included: a row is only ever
    // resolved within the device it belongs to, so a write can never reassign another
    // tablet's row to the writer. Reading across devices is a separate concern — see the
    // `device_filter` the fetch functions below still take.
    let by_id = sqlx::query!(
        "SELECT id, version, last_modified FROM configs \
         WHERE id = $1 AND user_id = $2 AND device_uuid = $3",
        submitted_id,
        user_id,
        device_uuid
    )
    .fetch_optional(&mut **tx)
    .await?;

    // `id` is a global PRIMARY KEY, but both lookups above are scoped to this user and,
    // outside the cloud dashboard, to this device — so neither sees a row on the account's
    // other tablet that already holds the submitted id. Probe globally, or the write below
    // trips `configs_pkey`.
    let id_taken = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM configs WHERE id = $1)",
        submitted_id
    )
    .fetch_one(&mut **tx)
    .await?
    .unwrap_or(false);

    let existing = by_key
        .map(|row| ExistingConfig {
            id: row.id,
            version: row.version,
            last_modified: row.last_modified,
        })
        .or_else(|| {
            by_id.map(|row| ExistingConfig {
                id: row.id,
                version: row.version,
                last_modified: row.last_modified,
            })
        });

    let new_id = match (&existing, id_taken) {
        // The write lands on a row we already own, so leave that row's id alone rather
        // than renaming it onto an id another row holds.
        (Some(row), true) => row.id,
        // Nothing of ours to update, and the submitted id belongs to some other row: this
        // device gets its own copy of the key under an id that is actually free.
        (None, true) => Uuid::new_v4(),
        _ => submitted_id,
    };

    Ok(ConfigTarget { existing, new_id })
}

/// Applies a resolved config write: updates the row the target points at, or inserts.
#[allow(clippy::too_many_arguments)]
async fn write_config(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    client_id: &Uuid,
    device_uuid: Uuid,
    target: &ConfigTarget,
    version: i32,
    is_deleted: bool,
    last_modified: i64,
    key: &str,
    value: &str,
) -> Result<(), AppError> {
    if let Some(ref existing) = target.existing {
        sqlx::query!(
            "UPDATE configs SET id = $1, device_uuid = $2, client_uuid = $3, version = $4, \
                 is_deleted = $5, last_modified = $6, sync_state = $7::text::sync_state, \
                 key = $8, value = $9 \
             WHERE id = $10 AND user_id = $11",
            target.new_id,
            device_uuid,
            client_id,
            version,
            is_deleted,
            last_modified,
            "SYNCED",
            key,
            value,
            existing.id,
            user_id
        )
        .execute(&mut **tx)
        .await?;
    } else {
        sqlx::query!(
            "INSERT INTO configs (id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state, key, value) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8::text::sync_state, $9, $10)",
            target.new_id,
            user_id,
            device_uuid,
            client_id,
            version,
            is_deleted,
            last_modified,
            "SYNCED",
            key,
            value
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn process_config_changes(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    client_id: &Uuid,
    device_rule: &ItemDeviceRule,
    device_filter: Option<Uuid>,
    changes: &[ConfigChangeDelta],
    success_ids: &mut Vec<String>,
    upload_status: &mut Vec<SuccessResult>,
    remote_changes: &mut Vec<ConfigChangeDelta>,
) -> Result<(), AppError> {
    for change in changes {
        let change_id = &change.id;
        let change_uuid = super::remote_mutations::parse_or_hash_uuid(change_id);
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!("Processing config {}", change_id);

                let is_need_update = matches!(change.operation_type, OperationType::Update)
                    && (change.data.is_none() || change.data.as_ref().map(|v| v.is_null()).unwrap_or(false));

                if is_need_update {
                    let existing = sqlx::query!(
                        "SELECT id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state::TEXT as sync_state, key, value \
                         FROM configs WHERE id = $1 AND user_id = $2 AND ($3::uuid IS NULL OR device_uuid = $3)",
                        change_uuid,
                        user_id,
                        device_filter
                    )
                    .fetch_optional(&mut **tx)
                    .await?;

                    if let Some(row) = existing {
                        let item_data = ConfigData {
                            id: row.id,
                            user_id: row.user_id.to_string(),
                            client_uuid: row.client_uuid.to_string(),
                            device_uuid: Some(row.device_uuid),
                            version: row.version,
                            is_deleted: row.is_deleted,
                            last_modified: row.last_modified,
                            sync_state: row.sync_state.clone().unwrap_or_else(|| "SYNCED".to_string()),
                            key: row.key,
                            value: row.value,
                        };
                        let data_val = serde_json::to_value(&item_data)?;
                        remote_changes.push(ConfigChangeDelta {
                            id: change_id.to_string(),
                            operation_type: OperationType::Update,
                            version: row.version,
                            device_uuid: Some(row.device_uuid),
                            data: Some(data_val),
                        });
                        success_ids.push(change_id.to_string());
                    }
                    continue;
                }

                if let Some(ref data) = change.data {
                    match serde_json::from_value::<ConfigData>(data.clone()) {
                        Ok(item) => {
                            let device_uuid = resolve_item_device(
                                tx,
                                user_id,
                                change.device_uuid.or(item.device_uuid),
                                device_rule,
                                "Config",
                                change_id,
                            )
                            .await?;

                            let target = resolve_config_target(
                                tx,
                                user_id,
                                device_uuid,
                                change_uuid,
                                &item.key,
                            )
                            .await?;

                            let next_version = if let Some(ref row) = target.existing {
                                if item.version == row.version {
                                    row.version + 1
                                } else if item.version < row.version {
                                    if item.last_modified >= row.last_modified {
                                        tracing::warn!(
                                            "MVCC Conflict for config {}. Client version: {}, Server version: {}. Resolving via LWW (Client wins: client last_modified {} >= server last_modified {}). Overwriting server state.",
                                            change_id, item.version, row.version, item.last_modified, row.last_modified
                                        );
                                        row.version + 1
                                    } else {
                                        tracing::warn!(
                                            "MVCC Conflict for config {}. Client version: {}, Server version: {}. Resolving via LWW (Server wins: client last_modified {} < server last_modified {}). Rejecting client update.",
                                            change_id, item.version, row.version, item.last_modified, row.last_modified
                                        );
                                        // Server has a newer write. Reject incoming update, return current server state version
                                        upload_status.push(SuccessResult {
                                            id: change_id.to_string(),
                                            version: row.version,
                                            sync_state: "SYNCED".to_string(),
                                        });
                                        success_ids.push(change_id.to_string());
                                        continue;
                                    }
                                } else {
                                    // Client is ahead
                                    item.version + 1
                                }
                            } else {
                                item.version
                            };

                            tracing::info!(
                                "Applying config upsert for {} (key: {}, device: {}). Version: {}, is_deleted: {}",
                                change_id,
                                item.key,
                                device_uuid,
                                next_version,
                                item.is_deleted
                            );

                            write_config(
                                tx,
                                user_id,
                                client_id,
                                device_uuid,
                                &target,
                                next_version,
                                item.is_deleted,
                                item.last_modified,
                                &item.key,
                                &item.value,
                            )
                            .await?;

                            upload_status.push(SuccessResult {
                                id: change_id.to_string(),
                                version: next_version,
                                sync_state: "SYNCED".to_string(),
                            });
                            success_ids.push(change_id.to_string());
                        }
                        Err(err) => {
                            tracing::error!("Failed to deserialize ConfigData for config {}: {:?}. Data: {:?}", change_id, err, data);
                            return Err(AppError::Serialization(err));
                        }
                    }
                } else if matches!(change.operation_type, OperationType::Update) {
                    let device_uuid = resolve_item_device(
                        tx,
                        user_id,
                        change.device_uuid,
                        device_rule,
                        "Config",
                        change_id,
                    )
                    .await?;

                    let existing = sqlx::query!(
                        "SELECT version FROM configs WHERE id = $1 AND user_id = $2 AND device_uuid = $3",
                        change_uuid,
                        user_id,
                        device_uuid
                    )
                    .fetch_optional(&mut **tx)
                    .await?;

                    if let Some(row) = existing {
                        let next_version = row.version + 1;
                        tracing::info!("Applying config metadata update for {}. Next version: {}", change_id, next_version);
                        sqlx::query!(
                            "UPDATE configs SET version = $1, client_uuid = $2, sync_state = 'SYNCED' \
                             WHERE id = $3 AND user_id = $4 AND device_uuid = $5",
                            next_version,
                            client_id,
                            change_uuid,
                            user_id,
                            device_uuid
                        )
                        .execute(&mut **tx)
                        .await?;

                        upload_status.push(SuccessResult {
                            id: change_id.to_string(),
                            version: next_version,
                            sync_state: "SYNCED".to_string(),
                        });
                        success_ids.push(change_id.to_string());
                    }
                }
            }
            OperationType::Delete => {
                let device_uuid = resolve_item_device(
                    tx,
                    user_id,
                    change.device_uuid,
                    device_rule,
                    "Config",
                    change_id,
                )
                .await?;

                let existing = sqlx::query!(
                    "SELECT version FROM configs WHERE id = $1 AND user_id = $2 AND device_uuid = $3",
                    change_uuid,
                    user_id,
                    device_uuid
                )
                .fetch_optional(&mut **tx)
                .await?;

                if let Some(row) = existing {
                    let next_version = row.version + 1;
                    tracing::info!("Applying config soft-delete for {}. Next version: {}", change_id, next_version);
                    sqlx::query!(
                        "UPDATE configs SET is_deleted = TRUE, version = $1, client_uuid = $2, sync_state = 'PENDING_DELETE' \
                         WHERE id = $3 AND user_id = $4 AND device_uuid = $5",
                        next_version,
                        client_id,
                        change_uuid,
                        user_id,
                        device_uuid
                    )
                    .execute(&mut **tx)
                    .await?;

                    upload_status.push(SuccessResult {
                        id: change_id.to_string(),
                        version: next_version,
                        sync_state: "SYNCED".to_string(),
                    });
                    success_ids.push(change_id.to_string());
                }
            }
        }
    }
    Ok(())
}

pub async fn fetch_remote_config_mutations(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    client_id: &Uuid,
    device_filter: Option<Uuid>,
    last_synced_at: Option<DateTime<Utc>>,
) -> Result<Vec<ConfigChangeDelta>, AppError> {
    let mut remote_changes = Vec::new();
    let is_initial_sync = last_synced_at.is_none() || last_synced_at.map(|t| t.timestamp() <= 0).unwrap_or(true);
    let last_synced_ms = last_synced_at.map(|t| t.timestamp_millis()).unwrap_or(0);

    let rows = sqlx::query!(
        "SELECT id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state::TEXT as sync_state, key, value \
         FROM configs \
         WHERE user_id = $1 AND last_modified > $2 AND ($4 OR client_uuid != $3) AND ($4 = FALSE OR is_deleted = FALSE) \
           AND ($5::uuid IS NULL OR device_uuid = $5)",
        user_id,
        last_synced_ms,
        client_id,
        is_initial_sync,
        device_filter
    )
    .fetch_all(&mut **tx)
    .await?;

    for row in rows {
        let item_data = ConfigData {
            id: row.id,
            user_id: row.user_id.to_string(),
            client_uuid: row.client_uuid.to_string(),
            device_uuid: Some(row.device_uuid),
            version: row.version,
            is_deleted: row.is_deleted,
            last_modified: row.last_modified,
            sync_state: row.sync_state.clone().unwrap_or_else(|| "SYNCED".to_string()),
            key: row.key,
            value: row.value,
        };

        let data_val = serde_json::to_value(&item_data)?;

        remote_changes.push(ConfigChangeDelta {
            id: row.id.to_string(),
            operation_type: if row.is_deleted {
                OperationType::Delete
            } else {
                OperationType::Update
            },
            version: row.version,
            device_uuid: Some(row.device_uuid),
            data: Some(data_val),
        });
    }

    Ok(remote_changes)
}

#[allow(clippy::too_many_arguments)]
pub async fn process_config_sync_items(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    client_id: &Uuid,
    device_rule: &ItemDeviceRule,
    items: &[ConfigSyncItem],
    success_uuids: &mut Vec<Uuid>,
) -> Result<(), AppError> {
    for item in items {
        let is_delete = item.is_deleted || item.sync_state == "PENDING_DELETE";

        // Resolved before the split: a delete is a write too, so it is device-scoped on
        // the same terms as an upsert.
        let device_uuid = resolve_item_device(
            tx,
            user_id,
            item.device_uuid,
            device_rule,
            "Config",
            &item.id.to_string(),
        )
        .await?;

        if is_delete {
            let existing = sqlx::query!(
                "SELECT version FROM configs WHERE id = $1 AND user_id = $2 AND device_uuid = $3",
                item.id,
                user_id,
                device_uuid
            )
            .fetch_optional(&mut **tx)
            .await?;

            if let Some(row) = existing {
                let next_version = row.version + 1;
                tracing::info!("Applying config soft-delete for {}. Next version: {}", item.id, next_version);
                sqlx::query!(
                    "UPDATE configs SET is_deleted = TRUE, version = $1, client_uuid = $2, sync_state = 'PENDING_DELETE'::text::sync_state \
                     WHERE id = $3 AND user_id = $4 AND device_uuid = $5",
                    next_version,
                    client_id,
                    item.id,
                    user_id,
                    device_uuid
                )
                .execute(&mut **tx)
                .await?;
            }
            success_uuids.push(item.id);
        } else {
            let target =
                resolve_config_target(tx, user_id, device_uuid, item.id, &item.key)
                    .await?;

            let next_version = if let Some(ref row) = target.existing {
                if item.version == row.version {
                    row.version + 1
                } else if item.version < row.version {
                    if item.last_modified >= row.last_modified {
                        tracing::warn!(
                            "MVCC Conflict for config {}. Client version: {}, Server version: {}. Resolving via LWW (Client wins: client last_modified {} >= server last_modified {}). Overwriting server state.",
                            item.id, item.version, row.version, item.last_modified, row.last_modified
                        );
                        row.version + 1
                    } else {
                        tracing::warn!(
                            "MVCC Conflict for config {}. Client version: {}, Server version: {}. Resolving via LWW (Server wins: client last_modified {} < server last_modified {}). Rejecting client update.",
                            item.id, item.version, row.version, item.last_modified, row.last_modified
                        );
                        // Server has a newer write. Accept the server's state, but count as success so client receives server state
                        success_uuids.push(item.id);
                        continue;
                    }
                } else {
                    item.version + 1
                }
            } else {
                item.version
            };

            tracing::info!(
                "Applying config upsert for {} (key: {}, device: {}). Next version: {}, is_deleted: {}",
                item.id,
                item.key,
                device_uuid,
                next_version,
                item.is_deleted
            );

            write_config(
                tx,
                user_id,
                client_id,
                device_uuid,
                &target,
                next_version,
                item.is_deleted,
                item.last_modified,
                &item.key,
                &item.value,
            )
            .await?;

            success_uuids.push(item.id);
            // The server's id won the reconciliation, so echo that row back too — it is how
            // the client learns which row its key actually lives on.
            if target.new_id != item.id {
                success_uuids.push(target.new_id);
            }
        }
    }
    Ok(())
}

pub async fn fetch_configs_for_response(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    client_id: &Uuid,
    device_filter: Option<Uuid>,
    last_synced_at: Option<DateTime<Utc>>,
    success_uuids: &[Uuid],
) -> Result<Vec<ConfigSyncItem>, AppError> {
    let is_initial_sync = last_synced_at.is_none() || last_synced_at.map(|t| t.timestamp() <= 0).unwrap_or(true);
    let last_synced_ms = last_synced_at.map(|t| t.timestamp_millis()).unwrap_or(0);

    let rows = sqlx::query!(
        "SELECT id, device_uuid, version, is_deleted, last_modified, sync_state::TEXT as sync_state, key, value \
         FROM configs \
         WHERE user_id = $1 AND ((last_modified > $2 AND ($5 OR client_uuid != $3)) OR id = ANY($4)) \
           AND ($6::uuid IS NULL OR device_uuid = $6)",
        user_id,
        last_synced_ms,
        client_id,
        success_uuids,
        is_initial_sync,
        device_filter
    )
    .fetch_all(&mut **tx)
    .await?;

    let items = rows
        .into_iter()
        .map(|row| ConfigSyncItem {
            id: row.id,
            device_uuid: Some(row.device_uuid),
            key: row.key,
            value: row.value,
            sync_state: row.sync_state.unwrap_or_else(|| "SYNCED".to_string()),
            version: row.version,
            is_deleted: row.is_deleted,
            last_modified: row.last_modified,
        })
        .collect();

    Ok(items)
}
