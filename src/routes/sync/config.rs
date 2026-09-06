use crate::routes::sync::device::{
    ItemDeviceRule, registered_device_set, resolve_item_device_cached,
};
use crate::routes::sync::batching::RunTracker;
use crate::routes::sync::deletes::ack_unsynced_delete;
use crate::routes::sync::paging::{probe_limit, trim_page, trim_size, Page};
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

/// The kinds of write these processors issue. A run may only contain one of them; see
/// `crate::routes::sync::batching`.
#[derive(PartialEq, Eq)]
enum WriteKind {
    Insert,
    UpdateInPlace,
    VersionBump,
    Delete,
}

/// Everything a buffered config write invalidates, in the form the run tracker compares.
///
/// These are exactly the four things [`ConfigBatch::note_write`] marks stale, and for the
/// same reason: an update can move a row onto a different id *and* a different key, so the
/// pair it vacates has to bound a run as much as the pair it lands on. Two rows in one
/// statement that swap keys would otherwise be checked against
/// `unique_user_device_config_key` together and trip it, where the per-item writes they
/// replace would have gone through one at a time.
fn write_tokens(device_uuid: Uuid, target: &ConfigTarget, key: &str) -> Vec<String> {
    let mut tokens = vec![
        format!("id:{}", target.new_id),
        format!("key:{}/{}", device_uuid, key),
    ];
    if let Some(ref existing) = target.existing {
        tokens.push(format!("id:{}", existing.id));
        tokens.push(format!("key:{}/{}", device_uuid, existing.key));
    }
    tokens
}

/// The column vectors for the run being accumulated. Columns that are the same for every
/// row this request writes — `user_id`, `client_uuid`, `last_modified` — stay scalar.
#[derive(Default)]
struct ConfigPending {
    ins_id: Vec<Uuid>,
    ins_device_uuid: Vec<Uuid>,
    ins_version: Vec<i32>,
    ins_is_deleted: Vec<bool>,
    ins_client_last_modified: Vec<i64>,
    ins_key: Vec<String>,
    ins_value: Vec<String>,

    upd_id: Vec<Uuid>,
    upd_device_uuid: Vec<Uuid>,
    upd_version: Vec<i32>,
    upd_is_deleted: Vec<bool>,
    upd_client_last_modified: Vec<i64>,
    upd_key: Vec<String>,
    upd_value: Vec<String>,

    bump_id: Vec<Uuid>,
    bump_version: Vec<i32>,
    bump_device_uuid: Vec<Uuid>,

    del_id: Vec<Uuid>,
    del_version: Vec<i32>,
    del_device_uuid: Vec<Uuid>,
}

impl ConfigPending {
    fn is_empty(&self) -> bool {
        self.ins_id.is_empty()
            && self.upd_id.is_empty()
            && self.bump_id.is_empty()
            && self.del_id.is_empty()
    }

    /// Issues the buffered run.
    ///
    /// The two `RETURNING` statements append to `broadcasts` here rather than in the loop,
    /// so a run's broadcasts are ordered among themselves by what the statement returned
    /// rather than by arrival. That is safe because a run cannot contain two writes to the
    /// same `(device, key)` — `write_tokens` makes that a run boundary — and a broadcast is
    /// an overwrite keyed on exactly that pair, so no listener can observe the difference.
    async fn flush(
        &mut self,
        tx: &mut Transaction<'_, Postgres>,
        user_id: &Uuid,
        client_id: &Uuid,
        server_ms: i64,
        broadcasts: &mut Vec<ConfigBroadcast>,
    ) -> Result<(), AppError> {
        if !self.ins_id.is_empty() {
            sqlx::query!(
                "INSERT INTO configs (id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, client_last_modified, sync_state, key, value) \
                 SELECT v.id, $1, v.device_uuid, $2, v.version, v.is_deleted, $3, v.client_last_modified, 'SYNCED'::text::sync_state, v.key, v.value \
                 FROM UNNEST($4::uuid[], $5::uuid[], $6::int4[], $7::bool[], $8::int8[], $9::text[], $10::text[]) \
                      AS v(id, device_uuid, version, is_deleted, client_last_modified, key, value)",
                user_id,
                client_id,
                server_ms,
                &self.ins_id,
                &self.ins_device_uuid,
                &self.ins_version,
                &self.ins_is_deleted,
                &self.ins_client_last_modified,
                &self.ins_key,
                &self.ins_value
            )
            .execute(&mut **tx)
            .await?;

            self.ins_id.clear();
            self.ins_device_uuid.clear();
            self.ins_version.clear();
            self.ins_is_deleted.clear();
            self.ins_client_last_modified.clear();
            self.ins_key.clear();
            self.ins_value.clear();
        }

        if !self.upd_id.is_empty() {
            // No `SET id` here, unlike the per-item statement this replaces: only writes
            // that keep the row's id are batched, and a write that renames one is issued
            // on its own. See `write_config`.
            sqlx::query!(
                "UPDATE configs SET device_uuid = v.device_uuid, client_uuid = $1, version = v.version, \
                     is_deleted = v.is_deleted, last_modified = $2, client_last_modified = v.client_last_modified, \
                     sync_state = 'SYNCED'::text::sync_state, key = v.key, value = v.value \
                 FROM UNNEST($3::uuid[], $4::uuid[], $5::int4[], $6::bool[], $7::int8[], $8::text[], $9::text[]) \
                      AS v(id, device_uuid, version, is_deleted, client_last_modified, key, value) \
                 WHERE configs.id = v.id AND configs.user_id = $10",
                client_id,
                server_ms,
                &self.upd_id,
                &self.upd_device_uuid,
                &self.upd_version,
                &self.upd_is_deleted,
                &self.upd_client_last_modified,
                &self.upd_key,
                &self.upd_value,
                user_id
            )
            .execute(&mut **tx)
            .await?;

            self.upd_id.clear();
            self.upd_device_uuid.clear();
            self.upd_version.clear();
            self.upd_is_deleted.clear();
            self.upd_client_last_modified.clear();
            self.upd_key.clear();
            self.upd_value.clear();
        }

        if !self.bump_id.is_empty() {
            let written = sqlx::query!(
                "UPDATE configs SET version = v.version, client_uuid = $1, last_modified = $2, sync_state = 'SYNCED' \
                 FROM UNNEST($3::uuid[], $4::int4[], $5::uuid[]) AS v(id, version, device_uuid) \
                 WHERE configs.id = v.id AND configs.user_id = $6 AND configs.device_uuid = v.device_uuid \
                 RETURNING configs.id, configs.device_uuid, configs.version, configs.is_deleted, configs.last_modified, configs.key, configs.value",
                client_id,
                server_ms,
                &self.bump_id,
                &self.bump_version,
                &self.bump_device_uuid,
                user_id
            )
            .fetch_all(&mut **tx)
            .await?;

            for row in written {
                broadcasts.push(ConfigBroadcast {
                    device_uuid: row.device_uuid,
                    item: ConfigSyncItem {
                        id: row.id,
                        device_uuid: Some(row.device_uuid),
                        key: row.key,
                        value: row.value,
                        sync_state: "SYNCED".to_string(),
                        version: row.version,
                        is_deleted: row.is_deleted,
                        last_modified: row.last_modified,
                    },
                });
            }

            self.bump_id.clear();
            self.bump_version.clear();
            self.bump_device_uuid.clear();
        }

        if !self.del_id.is_empty() {
            let written = sqlx::query!(
                "UPDATE configs SET is_deleted = TRUE, version = v.version, client_uuid = $1, last_modified = $2, \
                     sync_state = 'PENDING_DELETE'::text::sync_state \
                 FROM UNNEST($3::uuid[], $4::int4[], $5::uuid[]) AS v(id, version, device_uuid) \
                 WHERE configs.id = v.id AND configs.user_id = $6 AND configs.device_uuid = v.device_uuid \
                 RETURNING configs.id, configs.device_uuid, configs.version, configs.last_modified, configs.key, configs.value",
                client_id,
                server_ms,
                &self.del_id,
                &self.del_version,
                &self.del_device_uuid,
                user_id
            )
            .fetch_all(&mut **tx)
            .await?;

            for row in written {
                broadcasts.push(ConfigBroadcast {
                    device_uuid: row.device_uuid,
                    item: ConfigSyncItem {
                        id: row.id,
                        device_uuid: Some(row.device_uuid),
                        key: row.key,
                        value: row.value,
                        sync_state: "PENDING_DELETE".to_string(),
                        version: row.version,
                        is_deleted: true,
                        last_modified: row.last_modified,
                    },
                });
            }

            self.del_id.clear();
            self.del_version.clear();
            self.del_device_uuid.clear();
        }

        Ok(())
    }
}

/// Applies a resolved config write: updates the row the target points at, or inserts.
#[allow(clippy::too_many_arguments)]
async fn write_config(
    tx: &mut Transaction<'_, Postgres>,
    // Invalidated here rather than at the call sites: a write whose effect the cache never
    // hears about is the one bug this whole arrangement has to not have.
    batch: &mut ConfigBatch,
    // Buffered here rather than issued, for the same reason: both call sites write through
    // this function, so this is the one place that has to get the run boundaries right.
    pending: &mut ConfigPending,
    runs: &mut RunTracker<WriteKind>,
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
    let tokens = write_tokens(device_uuid, target, key);

    match target.existing {
        // A write that moves the row onto a different id is issued on its own. Batching it
        // would mean a `SET id` inside a set-based `UPDATE`, where one row can rename
        // itself onto an id a sibling row in the same statement is vacating — a
        // `configs_pkey` violation that the per-item writes, applied one at a time, do not
        // have. The reconciliation only fires when the client's id and the server's
        // disagree, so this is the rare path paying the old cost. See `choose_new_id`.
        Some(ref existing) if existing.id != target.new_id => {
            pending
                .flush(tx, user_id, client_id, server_ms, broadcasts)
                .await?;
            runs.clear();

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
        }
        Some(ref existing) => {
            if tokens
                .iter()
                .any(|token| runs.needs_flush(&WriteKind::UpdateInPlace, token))
            {
                pending
                    .flush(tx, user_id, client_id, server_ms, broadcasts)
                    .await?;
                runs.clear();
            }
            pending.upd_id.push(existing.id);
            pending.upd_device_uuid.push(device_uuid);
            pending.upd_version.push(version);
            pending.upd_is_deleted.push(is_deleted);
            pending.upd_client_last_modified.push(client_last_modified);
            pending.upd_key.push(key.to_string());
            pending.upd_value.push(value.to_string());
            for token in tokens {
                runs.record(WriteKind::UpdateInPlace, token);
            }
        }
        None => {
            if tokens
                .iter()
                .any(|token| runs.needs_flush(&WriteKind::Insert, token))
            {
                pending
                    .flush(tx, user_id, client_id, server_ms, broadcasts)
                    .await?;
                runs.clear();
            }
            pending.ins_id.push(target.new_id);
            pending.ins_device_uuid.push(device_uuid);
            pending.ins_version.push(version);
            pending.ins_is_deleted.push(is_deleted);
            pending.ins_client_last_modified.push(client_last_modified);
            pending.ins_key.push(key.to_string());
            pending.ins_value.push(value.to_string());
            for token in tokens {
                runs.record(WriteKind::Insert, token);
            }
        }
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

    let mut pending = ConfigPending::default();
    let mut runs: RunTracker<WriteKind> = RunTracker::new();

    for change in changes {
        let change_id = &change.id;
        let change_uuid = super::remote_mutations::parse_or_hash_uuid(change_id);

        // Before any of the branches below reads this row. `ConfigBatch` marks a written id
        // stale and sends the next lookup for it back to the database, so a buffered write
        // has to have landed by then or that lookup returns the row as it was before it.
        // See `RunTracker::contains`.
        if runs.contains(&format!("id:{}", change_uuid)) {
            pending
                .flush(tx, user_id, client_id, server_ms, broadcasts)
                .await?;
            runs.clear();
        }

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

                            // The id guard above cannot cover this one: the key is only
                            // known now, and `resolve_config_target` looks the row up by
                            // `(device, key)` as well as by id.
                            if runs.contains(&format!("key:{}/{}", device_uuid, item.key)) {
                                pending
                                    .flush(tx, user_id, client_id, server_ms, broadcasts)
                                    .await?;
                                runs.clear();
                            }

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
                                &mut pending,
                                &mut runs,
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
                            return Err(crate::routes::sync::rejections::item_payload_rejected(
                                "config",
                                &change_id.to_string(),
                                &err,
                            ));
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
                        // The broadcast for this row is built from what the statement
                        // returns, so it is appended when the run flushes rather than here.
                        let token = format!("id:{}", change_uuid);
                        let key_token = format!("key:{}/{}", device_uuid, row.key);
                        if runs.needs_flush(&WriteKind::VersionBump, &token)
                            || runs.needs_flush(&WriteKind::VersionBump, &key_token)
                        {
                            pending
                                .flush(tx, user_id, client_id, server_ms, broadcasts)
                                .await?;
                            runs.clear();
                        }
                        pending.bump_id.push(change_uuid);
                        pending.bump_version.push(next_version);
                        pending.bump_device_uuid.push(device_uuid);
                        runs.record(WriteKind::VersionBump, token);
                        runs.record(WriteKind::VersionBump, key_token);

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
                    // As with the version bump above, the broadcast is built from the
                    // statement's own output and so is appended at flush time.
                    let token = format!("id:{}", change_uuid);
                    let key_token = format!("key:{}/{}", device_uuid, row.key);
                    if runs.needs_flush(&WriteKind::Delete, &token)
                        || runs.needs_flush(&WriteKind::Delete, &key_token)
                    {
                        pending
                            .flush(tx, user_id, client_id, server_ms, broadcasts)
                            .await?;
                        runs.clear();
                    }
                    pending.del_id.push(change_uuid);
                    pending.del_version.push(next_version);
                    pending.del_device_uuid.push(device_uuid);
                    runs.record(WriteKind::Delete, token);
                    runs.record(WriteKind::Delete, key_token);

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
    // The last run has no successor to trigger it.
    if !pending.is_empty() {
        pending
            .flush(tx, user_id, client_id, server_ms, broadcasts)
            .await?;
        runs.clear();
    }

    Ok(())
}

/// One page of the configs a client is owed, read from Postgres exactly once.
///
/// The same overlap `DrawingDownload` describes, one table over and three orders of
/// magnitude lighter per row: `fetch_remote_config_mutations` and
/// `fetch_configs_for_response` ran predicates where the second was a superset of the
/// first, so every config on every Scribble sync was read twice and emitted twice. Both
/// wire fields are still populated and still identical to what they were.
pub struct ConfigDownload {
    pub remote_changes: Vec<ConfigChangeDelta>,
    pub items: Vec<ConfigSyncItem>,
    /// `Some(ms)` when the page was cut short — see `crate::routes::sync::paging`.
    pub next_cursor_ms: Option<i64>,
}

/// Reads at most one page of the configs changed since the client's cursor.
pub async fn fetch_config_download(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    client_id: &Uuid,
    device_filter: Option<Uuid>,
    last_synced_at: Option<DateTime<Utc>>,
    // `None` serves the whole download in one reply, for a client that cannot resume a
    // truncated one. See `SyncRequest::supports_paging`.
    page_size: Option<usize>,
) -> Result<ConfigDownload, AppError> {
    let is_initial_sync = last_synced_at.is_none() || last_synced_at.map(|t| t.timestamp() <= 0).unwrap_or(true);
    let last_synced_ms = last_synced_at.map(|t| t.timestamp_millis()).unwrap_or(0);

    let probe_limit = probe_limit(page_size);
    let mut rows = fetch_config_page(tx, user_id, client_id, device_filter, last_synced_ms, None, is_initial_sync, probe_limit).await?;

    let next_cursor_ms = match trim_page(&mut rows, trim_size(page_size), |row| row.last_modified) {
        Page::Complete => None,
        Page::Truncated { next_cursor_ms } => Some(next_cursor_ms),
        Page::WholeMillisecond { ms } => {
            tracing::warn!(
                "More than a page of configs for user {} share last_modified {}; serving that millisecond whole",
                user_id, ms
            );
            rows = fetch_config_page(tx, user_id, client_id, device_filter, ms - 1, Some(ms), is_initial_sync, i64::MAX).await?;
            Some(ms)
        }
    };

    let mut remote_changes = Vec::with_capacity(rows.len());
    let mut items = Vec::with_capacity(rows.len());

    for row in rows {
        let sync_state = row.sync_state.unwrap_or_else(|| "SYNCED".to_string());

        // `configs` carries soft-deleted rows on an initial sync and
        // `remote_config_changes` suppresses them. Preserved rather than unified: it is
        // the wire as the clients have always seen it.
        if !(is_initial_sync && row.is_deleted) {
            let item_data = ConfigData {
                id: row.id,
                user_id: row.user_id.to_string(),
                client_uuid: row.client_uuid.to_string(),
                device_uuid: Some(row.device_uuid),
                version: row.version,
                is_deleted: row.is_deleted,
                last_modified: row.last_modified,
                sync_state: sync_state.clone(),
                key: row.key.clone(),
                value: row.value.clone(),
            };

            remote_changes.push(ConfigChangeDelta {
                id: row.id.to_string(),
                operation_type: if row.is_deleted {
                    OperationType::Delete
                } else {
                    OperationType::Update
                },
                version: row.version,
                device_uuid: Some(row.device_uuid),
                data: Some(serde_json::to_value(&item_data)?),
            });
        }

        items.push(ConfigSyncItem {
            id: row.id,
            device_uuid: Some(row.device_uuid),
            key: row.key,
            value: row.value,
            sync_state,
            version: row.version,
            is_deleted: row.is_deleted,
            last_modified: row.last_modified,
        });
    }

    Ok(ConfigDownload { remote_changes, items, next_cursor_ms })
}

/// The one download query, shared by the page read and the whole-millisecond re-read.
#[allow(clippy::too_many_arguments)]
async fn fetch_config_page(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    client_id: &Uuid,
    device_filter: Option<Uuid>,
    after_ms: i64,
    through_ms: Option<i64>,
    is_initial_sync: bool,
    limit: i64,
) -> Result<Vec<ConfigRow>, AppError> {
    let rows = sqlx::query_as!(
        ConfigRow,
        "SELECT id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state::TEXT as sync_state, key, value \
         FROM configs \
         WHERE user_id = $1 AND last_modified > $2 AND ($4 OR client_uuid != $3) \
           AND ($5::uuid IS NULL OR device_uuid = $5) \
           AND ($6::bigint IS NULL OR last_modified <= $6) \
         ORDER BY last_modified ASC, id ASC \
         LIMIT $7",
        user_id,
        after_ms,
        client_id,
        is_initial_sync,
        device_filter,
        through_ms,
        limit
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows)
}

struct ConfigRow {
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
    // See `process_drawing_sync_items`: the flat upload paths were the only ones that
    // reported no per-item version in `upload_status`, which is why their echo has to
    // carry whole rows back. Reporting it here is the first half of fixing that.
    upload_status: &mut Vec<SuccessResult>,
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

    let mut pending = ConfigPending::default();
    let mut runs: RunTracker<WriteKind> = RunTracker::new();

    for item in items {
        let is_delete = item.is_deleted || item.sync_state == "PENDING_DELETE";

        // Same reason as `process_config_changes`: land any buffered write for this id
        // before the stale-marked cache goes back to the database for it. The matching key
        // guard has to wait until the device is resolved, just below.
        if runs.contains(&format!("id:{}", item.id)) {
            pending
                .flush(tx, user_id, client_id, server_ms, broadcasts)
                .await?;
            runs.clear();
        }

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

        // Now that the row's real device is known: a buffered write on this `(device, key)`
        // has to be in the database before anything below looks the pair up, or a second
        // item on the same key resolves to "no such row" and inserts a duplicate that
        // `unique_user_device_config_key` then rejects.
        if runs.contains(&format!("key:{}/{}", device_uuid, item.key)) {
            pending
                .flush(tx, user_id, client_id, server_ms, broadcasts)
                .await?;
            runs.clear();
        }

        if is_delete {
            let existing = batch.row_for(tx, user_id, item.id, Some(device_uuid)).await?;

            if let Some(row) = existing {
                batch.note_version_bump(device_uuid, item.id, &row.key);
                let next_version = advance_version("Config", &item.id.to_string(), row.version)?;
                tracing::info!("Applying config soft-delete for {}. Next version: {}", item.id, next_version);
                let token = format!("id:{}", item.id);
                let key_token = format!("key:{}/{}", device_uuid, row.key);
                if runs.needs_flush(&WriteKind::Delete, &token)
                    || runs.needs_flush(&WriteKind::Delete, &key_token)
                {
                    pending
                        .flush(tx, user_id, client_id, server_ms, broadcasts)
                        .await?;
                    runs.clear();
                }
                pending.del_id.push(item.id);
                pending.del_version.push(next_version);
                pending.del_device_uuid.push(device_uuid);
                runs.record(WriteKind::Delete, token);
                runs.record(WriteKind::Delete, key_token);

                upload_status.push(SuccessResult {
                    id: item.id.to_string(),
                    version: next_version,
                    sync_state: "SYNCED".to_string(),
                });
            } else {
                // Nothing to delete, so the delete has succeeded. See
                // `crate::routes::sync::deletes`.
                upload_status.push(SuccessResult {
                    id: item.id.to_string(),
                    version: ack_unsynced_delete("config", &item.id.to_string()),
                    sync_state: "SYNCED".to_string(),
                });
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
                &mut pending,
                &mut runs,
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
                id: item.id.to_string(),
                version: next_version,
                sync_state: "SYNCED".to_string(),
            });
            success_uuids.push(item.id);
            // The server's id won the reconciliation, so echo that row back too — it is how
            // the client learns which row its key actually lives on.
            if target.new_id != item.id {
                success_uuids.push(target.new_id);
            }
        }
    }
    // The last run has no successor to trigger it.
    if !pending.is_empty() {
        pending
            .flush(tx, user_id, client_id, server_ms, broadcasts)
            .await?;
        runs.clear();
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

/// The upload echo: the config rows this very request just wrote, read back.
///
/// Split out of the old combined query so that the download above can be paged without
/// the page limit ever swallowing a row the client is waiting on an acknowledgement for.
/// `success_uuids` is bounded by `DEFAULT_MAX_ITEMS_PER_COLLECTION`, so this needs no page
/// of its own. At 8 KiB a value the bytes hardly matter, but the duplicate read did.
pub async fn fetch_configs_for_echo(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    device_filter: Option<Uuid>,
    success_uuids: &[Uuid],
) -> Result<Vec<ConfigSyncItem>, AppError> {
    if success_uuids.is_empty() {
        return Ok(Vec::new());
    }

    let rows = sqlx::query!(
        "SELECT id, device_uuid, version, is_deleted, last_modified, sync_state::TEXT as sync_state, key, value \
         FROM configs \
         WHERE user_id = $1 AND id = ANY($2) AND ($3::uuid IS NULL OR device_uuid = $3)",
        user_id,
        success_uuids,
        device_filter
    )
    .fetch_all(&mut **tx)
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
