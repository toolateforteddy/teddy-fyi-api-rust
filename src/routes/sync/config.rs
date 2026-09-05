use crate::routes::sync::device::{
    ItemDeviceRule, registered_device_set, resolve_item_device_cached,
};
use crate::routes::sync::deletes::ack_unsynced_delete;
use crate::routes::sync::types::*;
use crate::routes::sync::versioning::{advance_version, seed_version};
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// A config row this request wrote, paired with the device whose stream should hear about it.
pub struct ConfigBroadcast {
    pub device_uuid: Uuid,
    pub item: ConfigSyncItem,
}

/// A config row already on the server that an incoming write should land on.
///
/// Its `version` is the sole input to the row's next version; the row's stored
/// `last_modified` is not read here at all any more, because nothing compares it — see
/// [`crate::routes::sync::versioning`].
struct ExistingConfig {
    id: Uuid,
    version: i32,
    /// The key the row currently sits under. Only the batch cache reads it: a write can
    /// move a row from one key to another, and the cache has to know which key it just
    /// invalidated. See [`ConfigBatch`].
    key: String,
}

/// A `configs` row as the write paths need to see it.
///
/// The column list is the widest any of them reads (the need-update path in
/// [`process_config_changes`], which echoes the whole row back); the narrower paths simply
/// ignore the rest. One shape means one prefetch and one fallback statement instead of
/// three of each.
#[derive(Clone)]
struct CachedConfig {
    id: Uuid,
    user_id: Uuid,
    device_uuid: Uuid,
    client_uuid: Uuid,
    version: i32,
    is_deleted: bool,
    last_modified: i64,
    sync_state: Option<String>,
    key: String,
    value: String,
}

/// Every `configs` lookup a batch of writes would otherwise make one item at a time.
///
/// [`resolve_config_target`] alone reads three things per item — the row under the
/// submitted id, the row under `(user, device, key)`, and whether the submitted id is taken
/// *anywhere* in the table — and the surrounding loop reads a fourth. At the ten thousand
/// items [`crate::routes::sync::limits`] allows in one collection that is forty thousand
/// sequential statements on one of sixteen pool connections, inside one transaction. Three
/// statements up front answer all of them.
///
/// # Why this is more than a `HashMap`
///
/// The loop *writes*, so the prefetch goes stale under it: two items in one payload can
/// contend for the same key, or for the same id. Rather than trying to patch the maps to
/// mirror every write — the id decision in [`choose_new_id`] depends on a global existence
/// probe, and an update can free the id and the key it used to hold — an id or key this
/// batch has touched is marked stale, and an item that hits one falls back to exactly the
/// per-item statements this type replaced. Intra-batch collisions are rare, so the old cost
/// is paid only where the old answer is the only correct one.
///
/// The bias is deliberate. Being wrong here is not a slow sync, it is an `INSERT` onto an id
/// another row already holds: a `configs_pkey` violation that fails the whole request.
struct ConfigBatch {
    /// Rows this account owns, by id. Populated from both prefetches.
    by_id: HashMap<Uuid, CachedConfig>,
    /// The same rows under their unique key. The value is the id; the row itself lives in
    /// `by_id`.
    by_key: HashMap<(Uuid, String), Uuid>,
    /// Ids that exist *anywhere* in `configs`, this account's rows included. Global on
    /// purpose — see [`resolve_config_target_uncached`].
    taken_ids: HashSet<Uuid>,
    /// What the prefetch actually asked about. A lookup outside these sets has no cached
    /// answer to give and must go to the database, so a caller that cannot work out an
    /// item's key up front (the change-delta path only learns it after deserializing) stays
    /// correct rather than reading a miss as "no such row".
    prefetched_ids: HashSet<Uuid>,
    prefetched_keys: HashSet<String>,
    /// Ids and `(device, key)` pairs this batch has already written.
    stale_ids: HashSet<Uuid>,
    stale_keys: HashSet<(Uuid, String)>,
}

impl ConfigBatch {
    async fn load(
        tx: &mut Transaction<'_, Postgres>,
        user_id: &Uuid,
        ids: &[Uuid],
        keys: &[String],
    ) -> Result<Self, AppError> {
        let mut batch = ConfigBatch {
            by_id: HashMap::new(),
            by_key: HashMap::new(),
            taken_ids: HashSet::new(),
            prefetched_ids: ids.iter().copied().collect(),
            prefetched_keys: keys.iter().cloned().collect(),
            stale_ids: HashSet::new(),
            stale_keys: HashSet::new(),
        };

        if !ids.is_empty() {
            let taken = sqlx::query_scalar!("SELECT id FROM configs WHERE id = ANY($1)", ids)
                .fetch_all(&mut **tx)
                .await?;
            batch.taken_ids = taken.into_iter().collect();

            let rows = sqlx::query!(
                "SELECT id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state::TEXT as sync_state, key, value \
                 FROM configs WHERE id = ANY($1) AND user_id = $2",
                ids,
                user_id
            )
            .fetch_all(&mut **tx)
            .await?;
            for row in rows {
                batch.remember(CachedConfig {
                    id: row.id,
                    user_id: row.user_id,
                    device_uuid: row.device_uuid,
                    client_uuid: row.client_uuid,
                    version: row.version,
                    is_deleted: row.is_deleted,
                    last_modified: row.last_modified,
                    sync_state: row.sync_state,
                    key: row.key,
                    value: row.value,
                });
            }
        }

        if !keys.is_empty() {
            // Not narrowed to the devices the batch names: the unique key is
            // `(user_id, device_uuid, key)`, and fetching an account's rows for these keys
            // across all of its devices costs at most one row per registered device — the
            // device cap keeps that a small multiple — while sparing the caller from having
            // to resolve every item's device before the loop that resolves devices.
            let rows = sqlx::query!(
                "SELECT id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state::TEXT as sync_state, key, value \
                 FROM configs WHERE user_id = $1 AND key = ANY($2)",
                user_id,
                keys
            )
            .fetch_all(&mut **tx)
            .await?;
            for row in rows {
                batch.remember(CachedConfig {
                    id: row.id,
                    user_id: row.user_id,
                    device_uuid: row.device_uuid,
                    client_uuid: row.client_uuid,
                    version: row.version,
                    is_deleted: row.is_deleted,
                    last_modified: row.last_modified,
                    sync_state: row.sync_state,
                    key: row.key,
                    value: row.value,
                });
            }
        }

        Ok(batch)
    }

    fn remember(&mut self, row: CachedConfig) {
        self.by_key.insert((row.device_uuid, row.key.clone()), row.id);
        self.by_id.insert(row.id, row);
    }

    /// Whether the maps can still answer for this id on their own.
    fn id_is_fresh(&self, id: &Uuid) -> bool {
        self.prefetched_ids.contains(id) && !self.stale_ids.contains(id)
    }

    /// Whether they can answer a full target resolution, which reads the key as well.
    fn target_is_fresh(&self, id: &Uuid, device_uuid: Uuid, key: &str) -> bool {
        self.id_is_fresh(id)
            && self.prefetched_keys.contains(key)
            && !self.stale_keys.contains(&(device_uuid, key.to_string()))
    }

    /// The row `id` names, within this account and — when the request is scoped to one
    /// device — that device. The predicate the per-item statements used, unchanged.
    async fn row_for(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        user_id: &Uuid,
        id: Uuid,
        device_filter: Option<Uuid>,
    ) -> Result<Option<CachedConfig>, AppError> {
        if self.id_is_fresh(&id) {
            return Ok(self
                .by_id
                .get(&id)
                .filter(|row| {
                    row.user_id == *user_id
                        && device_filter.is_none_or(|device| row.device_uuid == device)
                })
                .cloned());
        }

        let row = sqlx::query!(
            "SELECT id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state::TEXT as sync_state, key, value \
             FROM configs WHERE id = $1 AND user_id = $2 AND ($3::uuid IS NULL OR device_uuid = $3)",
            id,
            user_id,
            device_filter
        )
        .fetch_optional(&mut **tx)
        .await?;

        Ok(row.map(|row| CachedConfig {
            id: row.id,
            user_id: row.user_id,
            device_uuid: row.device_uuid,
            client_uuid: row.client_uuid,
            version: row.version,
            is_deleted: row.is_deleted,
            last_modified: row.last_modified,
            sync_state: row.sync_state,
            key: row.key,
            value: row.value,
        }))
    }

    /// Records that a row has been written, so every later item in the batch re-reads it
    /// instead of trusting the prefetch. Both the id and the key the row leaves behind are
    /// invalidated as well as the ones it lands on: an update can move a row onto a
    /// different id and a different key, freeing both of the old ones.
    fn note_write(&mut self, device_uuid: Uuid, target: &ConfigTarget, key: &str) {
        self.stale_ids.insert(target.new_id);
        self.stale_keys.insert((device_uuid, key.to_string()));
        if let Some(ref existing) = target.existing {
            self.stale_ids.insert(existing.id);
            self.stale_keys
                .insert((device_uuid, existing.key.clone()));
        }
    }

    /// Records a write that changed a row in place — a metadata update or a soft-delete.
    /// The row keeps its id and its key; only its version moved, which is enough to make
    /// the cached copy wrong.
    fn note_version_bump(&mut self, device_uuid: Uuid, id: Uuid, key: &str) {
        self.stale_ids.insert(id);
        self.stale_keys.insert((device_uuid, key.to_string()));
    }
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

/// The id the row carries after the write, given what is already on the server.
///
/// Split out only so the batched and unbatched resolutions cannot drift: this is the whole
/// id-reconciliation rule, and it is the part a mistake turns into a `configs_pkey`
/// violation.
fn choose_new_id(existing: &Option<ExistingConfig>, id_taken: bool, submitted_id: Uuid) -> Uuid {
    match (existing, id_taken) {
        // The write lands on a row we already own, so leave that row's id alone rather
        // than renaming it onto an id another row holds.
        (Some(row), true) => row.id,
        // Nothing of ours to update, and the submitted id belongs to some other row: this
        // device gets its own copy of the key under an id that is actually free.
        (None, true) => Uuid::new_v4(),
        _ => submitted_id,
    }
}

/// Resolves the write target from the batch prefetch, falling back to the per-item
/// statements when the prefetch has nothing trustworthy to say about this id or key.
async fn resolve_config_target(
    tx: &mut Transaction<'_, Postgres>,
    batch: &ConfigBatch,
    user_id: &Uuid,
    device_uuid: Uuid,
    submitted_id: Uuid,
    key: &str,
) -> Result<ConfigTarget, AppError> {
    if !batch.target_is_fresh(&submitted_id, device_uuid, key) {
        return resolve_config_target_uncached(tx, user_id, device_uuid, submitted_id, key).await;
    }

    let by_key = batch
        .by_key
        .get(&(device_uuid, key.to_string()))
        .and_then(|id| batch.by_id.get(id));

    // Both prefetches are scoped to this account, but `by_id` is not scoped to the device,
    // so the device predicate the per-item statement carried is applied here instead.
    let by_id = batch
        .by_id
        .get(&submitted_id)
        .filter(|row| row.user_id == *user_id && row.device_uuid == device_uuid);

    let existing = by_key.or(by_id).map(|row| ExistingConfig {
        id: row.id,
        version: row.version,
        key: row.key.clone(),
    });
    let id_taken = batch.taken_ids.contains(&submitted_id);
    let new_id = choose_new_id(&existing, id_taken, submitted_id);

    Ok(ConfigTarget { existing, new_id })
}

async fn resolve_config_target_uncached(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    device_uuid: Uuid,
    submitted_id: Uuid,
    key: &str,
) -> Result<ConfigTarget, AppError> {
    let by_key = sqlx::query!(
        "SELECT id, version, key FROM configs WHERE user_id = $1 AND device_uuid = $2 AND key = $3",
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
        "SELECT id, version, key FROM configs \
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
            key: row.key,
        })
        .or_else(|| {
            by_id.map(|row| ExistingConfig {
                id: row.id,
                version: row.version,
                key: row.key,
            })
        });

    let new_id = choose_new_id(&existing, id_taken, submitted_id);

    Ok(ConfigTarget { existing, new_id })
}

/// Applies a resolved config write: updates the row the target points at, or inserts.
#[allow(clippy::too_many_arguments)]
async fn write_config(
    tx: &mut Transaction<'_, Postgres>,
    // Invalidated here rather than at the call sites: a write whose effect the cache never
    // hears about is the one bug this whole arrangement has to not have.
    batch: &mut ConfigBatch,
    user_id: &Uuid,
    client_id: &Uuid,
    device_uuid: Uuid,
    target: &ConfigTarget,
    version: i32,
    is_deleted: bool,
    // The server's stamp for this write, and the client's own claim about when the edit
    // happened. Only the first is stored in `last_modified` — it is what the download
    // cursor and the conflict policy are defined in — and the second is kept beside it
    // where nothing compares it. See `crate::routes::sync::versioning`.
    server_ms: i64,
    client_last_modified: i64,
    key: &str,
    value: &str,
    broadcasts: &mut Vec<ConfigBroadcast>,
) -> Result<(), AppError> {
    if let Some(ref existing) = target.existing {
        sqlx::query!(
            "UPDATE configs SET id = $1, device_uuid = $2, client_uuid = $3, version = $4, \
                 is_deleted = $5, last_modified = $6, client_last_modified = $7, \
                 sync_state = $8::text::sync_state, key = $9, value = $10 \
             WHERE id = $11 AND user_id = $12",
            target.new_id,
            device_uuid,
            client_id,
            version,
            is_deleted,
            server_ms,
            client_last_modified,
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
            "INSERT INTO configs (id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, client_last_modified, sync_state, key, value) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::text::sync_state, $10, $11)",
            target.new_id,
            user_id,
            device_uuid,
            client_id,
            version,
            is_deleted,
            server_ms,
            client_last_modified,
            "SYNCED",
            key,
            value
        )
        .execute(&mut **tx)
        .await?;
    }

    batch.note_write(device_uuid, target, key);

    broadcasts.push(ConfigBroadcast {
        device_uuid,
        item: ConfigSyncItem {
            id: target.new_id,
            device_uuid: Some(device_uuid),
            key: key.to_string(),
            value: value.to_string(),
            sync_state: "SYNCED".to_string(),
            version,
            is_deleted,
            // The broadcast carries what was stored, so a listening stream and a polling
            // client end up holding the same stamp for the same row.
            last_modified: server_ms,
        },
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn process_config_changes(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    client_id: &Uuid,
    // This request's server clock reading, in epoch milliseconds. See `write_config`.
    server_ms: i64,
    device_rule: &ItemDeviceRule,
    device_filter: Option<Uuid>,
    changes: &[ConfigChangeDelta],
    success_ids: &mut Vec<String>,
    upload_status: &mut Vec<SuccessResult>,
    remote_changes: &mut Vec<ConfigChangeDelta>,
    broadcasts: &mut Vec<ConfigBroadcast>,
) -> Result<(), AppError> {
    // Everything the loop below would otherwise ask the database once per item. The ids are
    // exact; the keys and devices are read straight off the delta without deserializing it,
    // so they are a best-effort superset — an item whose key the peek missed simply resolves
    // the old way instead of trusting a cache miss. See `ConfigBatch`.
    let mut ids: Vec<Uuid> = Vec::new();
    let mut keys: HashSet<String> = HashSet::new();
    let mut devices: HashSet<Uuid> = HashSet::new();
    for change in changes {
        ids.push(super::remote_mutations::parse_or_hash_uuid(&change.id));
        if let Some(device_uuid) = change.device_uuid {
            devices.insert(device_uuid);
        }
        if let Some(ref data) = change.data {
            if let Some(key) = data.get("key").and_then(|v| v.as_str()) {
                keys.insert(key.to_string());
            }
            if let Some(device_uuid) = data
                .get("device_uuid")
                .and_then(|v| v.as_str())
                .and_then(|v| Uuid::parse_str(v).ok())
            {
                devices.insert(device_uuid);
            }
        }
    }
    let keys: Vec<String> = keys.into_iter().collect();
    let devices: Vec<Uuid> = devices.into_iter().collect();

    let registered = registered_device_set(tx, user_id, device_rule, &devices).await?;
    let mut batch = ConfigBatch::load(tx, user_id, &ids, &keys).await?;

    for change in changes {
        let change_id = &change.id;
        let change_uuid = super::remote_mutations::parse_or_hash_uuid(change_id);
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!("Processing config {}", change_id);

                let is_need_update = matches!(change.operation_type, OperationType::Update)
                    && (change.data.is_none() || change.data.as_ref().map(|v| v.is_null()).unwrap_or(false));

                if is_need_update {
                    let existing = batch.row_for(tx, user_id, change_uuid, device_filter).await?;

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
                            let device_uuid = resolve_item_device_cached(
                                tx,
                                user_id,
                                change.device_uuid.or(item.device_uuid),
                                device_rule,
                                "Config",
                                change_id,
                                &registered,
                            )
                            .await?;

                            let target = resolve_config_target(
                                tx,
                                &batch,
                                user_id,
                                device_uuid,
                                change_uuid,
                                &item.key,
                            )
                            .await?;

                            // Server row decides the number; the client's clock decides
                            // nothing. Identical to the drawing path, deliberately — see
                            // `crate::routes::sync::versioning` for the one policy both
                            // now follow.
                            let next_version = match target.existing {
                                Some(ref row) => {
                                    if item.version < row.version {
                                        tracing::warn!(
                                            "Conflicting write for config {} (client version {}, server version {}); accepting it as the later arrival",
                                            change_id, item.version, row.version
                                        );
                                    }
                                    advance_version("Config", change_id, row.version)?
                                }
                                None => seed_version("Config", change_id, item.version)?,
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
                                &mut batch,
                                user_id,
                                client_id,
                                device_uuid,
                                &target,
                                next_version,
                                item.is_deleted,
                                server_ms,
                                item.last_modified,
                                &item.key,
                                &item.value,
                                broadcasts,
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
                    let device_uuid = resolve_item_device_cached(
                        tx,
                        user_id,
                        change.device_uuid,
                        device_rule,
                        "Config",
                        change_id,
                        &registered,
                    )
                    .await?;

                    let existing = batch
                        .row_for(tx, user_id, change_uuid, Some(device_uuid))
                        .await?;

                    if let Some(row) = existing {
                        batch.note_version_bump(device_uuid, change_uuid, &row.key);
                        let next_version = advance_version("Config", change_id, row.version)?;
                        tracing::info!("Applying config metadata update for {}. Next version: {}", change_id, next_version);
                        // `last_modified` moves with the write: it is the cursor the
                        // account's other devices poll against, so a row that changes
                        // without it changing is a change they never see.
                        let written = sqlx::query!(
                            "UPDATE configs SET version = $1, client_uuid = $2, last_modified = $3, sync_state = 'SYNCED' \
                             WHERE id = $4 AND user_id = $5 AND device_uuid = $6 \
                             RETURNING device_uuid, is_deleted, last_modified, key, value",
                            next_version,
                            client_id,
                            server_ms,
                            change_uuid,
                            user_id,
                            device_uuid
                        )
                        .fetch_optional(&mut **tx)
                        .await?;

                        if let Some(row) = written {
                            broadcasts.push(ConfigBroadcast {
                                device_uuid: row.device_uuid,
                                item: ConfigSyncItem {
                                    id: change_uuid,
                                    device_uuid: Some(row.device_uuid),
                                    key: row.key,
                                    value: row.value,
                                    sync_state: "SYNCED".to_string(),
                                    version: next_version,
                                    is_deleted: row.is_deleted,
                                    last_modified: row.last_modified,
                                },
                            });
                        }

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
                let device_uuid = resolve_item_device_cached(
                    tx,
                    user_id,
                    change.device_uuid,
                    device_rule,
                    "Config",
                    change_id,
                    &registered,
                )
                .await?;

                let existing = batch
                    .row_for(tx, user_id, change_uuid, Some(device_uuid))
                    .await?;

                if let Some(row) = existing {
                    batch.note_version_bump(device_uuid, change_uuid, &row.key);
                    let next_version = advance_version("Config", change_id, row.version)?;
                    tracing::info!("Applying config soft-delete for {}. Next version: {}", change_id, next_version);
                    // Stamped for the same reason as the update above: a soft-delete the
                    // cursor cannot see is a deletion the sibling tablet never applies.
                    let written = sqlx::query!(
                        "UPDATE configs SET is_deleted = TRUE, version = $1, client_uuid = $2, last_modified = $3, sync_state = 'PENDING_DELETE' \
                         WHERE id = $4 AND user_id = $5 AND device_uuid = $6 \
                         RETURNING device_uuid, last_modified, key, value",
                        next_version,
                        client_id,
                        server_ms,
                        change_uuid,
                        user_id,
                        device_uuid
                    )
                    .fetch_optional(&mut **tx)
                    .await?;

                    if let Some(row) = written {
                        broadcasts.push(ConfigBroadcast {
                            device_uuid: row.device_uuid,
                            item: ConfigSyncItem {
                                id: change_uuid,
                                device_uuid: Some(row.device_uuid),
                                key: row.key,
                                value: row.value,
                                sync_state: "PENDING_DELETE".to_string(),
                                version: next_version,
                                is_deleted: true,
                                last_modified: row.last_modified,
                            },
                        });
                    }

                    upload_status.push(SuccessResult {
                        id: change_id.to_string(),
                        version: next_version,
                        sync_state: "SYNCED".to_string(),
                    });
                    success_ids.push(change_id.to_string());
                } else {
                    // Nothing to delete, so the delete has succeeded. Acknowledged rather
                    // than dropped: a change the response never mentions stays pending on
                    // the device and comes back on every sync forever. See
                    // `crate::routes::sync::deletes`.
                    upload_status.push(SuccessResult {
                        id: change_id.to_string(),
                        version: ack_unsynced_delete("config", change_id),
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
    // This request's server clock reading, in epoch milliseconds. See `write_config`.
    server_ms: i64,
    device_rule: &ItemDeviceRule,
    items: &[ConfigSyncItem],
    success_uuids: &mut Vec<Uuid>,
    broadcasts: &mut Vec<ConfigBroadcast>,
) -> Result<(), AppError> {
    // The flat list carries its ids, keys and devices in typed fields, so unlike the
    // change-delta path above the prefetch here is exact rather than a peek. See
    // `ConfigBatch` for what goes stale as the loop writes and how that is handled.
    let ids: Vec<Uuid> = items.iter().map(|item| item.id).collect();
    let keys: Vec<String> = items
        .iter()
        .map(|item| item.key.clone())
        .collect::<HashSet<String>>()
        .into_iter()
        .collect();
    let devices: Vec<Uuid> = items
        .iter()
        .filter_map(|item| item.device_uuid)
        .collect::<HashSet<Uuid>>()
        .into_iter()
        .collect();

    let registered = registered_device_set(tx, user_id, device_rule, &devices).await?;
    let mut batch = ConfigBatch::load(tx, user_id, &ids, &keys).await?;

    for item in items {
        let is_delete = item.is_deleted || item.sync_state == "PENDING_DELETE";

        // Resolved before the split: a delete is a write too, so it is device-scoped on
        // the same terms as an upsert.
        let device_uuid = resolve_item_device_cached(
            tx,
            user_id,
            item.device_uuid,
            device_rule,
            "Config",
            &item.id.to_string(),
            &registered,
        )
        .await?;

        if is_delete {
            let existing = batch.row_for(tx, user_id, item.id, Some(device_uuid)).await?;

            if let Some(row) = existing {
                batch.note_version_bump(device_uuid, item.id, &row.key);
                let next_version = advance_version("Config", &item.id.to_string(), row.version)?;
                tracing::info!("Applying config soft-delete for {}. Next version: {}", item.id, next_version);
                let written = sqlx::query!(
                    "UPDATE configs SET is_deleted = TRUE, version = $1, client_uuid = $2, last_modified = $3, \
                         sync_state = 'PENDING_DELETE'::text::sync_state \
                     WHERE id = $4 AND user_id = $5 AND device_uuid = $6 \
                     RETURNING device_uuid, last_modified, key, value",
                    next_version,
                    client_id,
                    server_ms,
                    item.id,
                    user_id,
                    device_uuid
                )
                .fetch_optional(&mut **tx)
                .await?;

                if let Some(row) = written {
                    broadcasts.push(ConfigBroadcast {
                        device_uuid: row.device_uuid,
                        item: ConfigSyncItem {
                            id: item.id,
                            device_uuid: Some(row.device_uuid),
                            key: row.key,
                            value: row.value,
                            sync_state: "PENDING_DELETE".to_string(),
                            version: next_version,
                            is_deleted: true,
                            last_modified: row.last_modified,
                        },
                    });
                }
            }
            success_uuids.push(item.id);
        } else {
            let target =
                resolve_config_target(tx, &batch, user_id, device_uuid, item.id, &item.key)
                    .await?;

            // Same policy as every other write path here.
            let next_version = match target.existing {
                Some(ref row) => {
                    if item.version < row.version {
                        tracing::warn!(
                            "Conflicting write for config {} (client version {}, server version {}); accepting it as the later arrival",
                            item.id, item.version, row.version
                        );
                    }
                    advance_version("Config", &item.id.to_string(), row.version)?
                }
                None => seed_version("Config", &item.id.to_string(), item.version)?,
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
                &mut batch,
                user_id,
                client_id,
                device_uuid,
                &target,
                next_version,
                item.is_deleted,
                server_ms,
                item.last_modified,
                &item.key,
                &item.value,
                broadcasts,
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

/// Every live config on the account, or on one device when `device_filter` names one.
///
/// This is the snapshot an SSE stream opens with: the state that the `DIRECT_UPDATE` events
/// following it are deltas against. Deleted rows are left out, since a stream that has seen
/// nothing yet has nothing to retract.
pub async fn fetch_config_snapshot(
    pool: &sqlx::PgPool,
    user_id: &Uuid,
    device_filter: Option<Uuid>,
) -> Result<Vec<ConfigSyncItem>, AppError> {
    let rows = sqlx::query!(
        "SELECT id, device_uuid, version, is_deleted, last_modified, sync_state::TEXT as sync_state, key, value \
         FROM configs \
         WHERE user_id = $1 AND is_deleted = FALSE AND ($2::uuid IS NULL OR device_uuid = $2)",
        user_id,
        device_filter
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
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
        .collect())
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
