use crate::observability::http::log_hash_salt_from_env;
use axum::{http::StatusCode, response::IntoResponse};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;



#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OperationType {
    Insert,
    Update,
    Delete,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TodoListChangeDelta {
    pub id: String,
    #[serde(rename = "type")]
    pub operation_type: OperationType,
    pub version: i32,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TodoChangeDelta {
    pub id: String,
    #[serde(rename = "type")]
    pub operation_type: OperationType,
    pub version: i32,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroceryListChangeDelta {
    pub id: String,
    #[serde(rename = "type")]
    pub operation_type: OperationType,
    pub version: i32,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroceryListMemberChangeDelta {
    pub id: String,
    #[serde(rename = "type")]
    pub operation_type: OperationType,
    pub version: i32,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StoreChangeDelta {
    pub id: String,
    #[serde(rename = "type")]
    pub operation_type: OperationType,
    pub version: i32,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CategoryChangeDelta {
    pub id: String,
    #[serde(rename = "type")]
    pub operation_type: OperationType,
    pub version: i32,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroceryChangeDelta {
    pub id: String,
    #[serde(rename = "type")]
    pub operation_type: OperationType,
    pub version: i32,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroceryItemStoreInfoChangeDelta {
    #[serde(default)]
    pub id: String,
    #[serde(alias = "groceryItemId")]
    pub grocery_item_id: String,
    #[serde(alias = "storeId")]
    pub store_id: String,
    #[serde(rename = "type")]
    pub operation_type: OperationType,
    pub version: i32,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ConfigChangeDelta {
    pub id: String,
    #[serde(rename = "type")]
    pub operation_type: OperationType,
    pub version: i32,
    /// Which tablet this row belongs to. Absent from pre-device clients; such a write
    /// falls back to the account's backfilled device.
    #[serde(default, alias = "deviceUuid")]
    pub device_uuid: Option<Uuid>,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DrawingChangeDelta {
    pub id: String,
    #[serde(rename = "type")]
    pub operation_type: OperationType,
    pub version: i32,
    /// Which tablet this row belongs to. Absent from pre-device clients; such a write
    /// falls back to the account's backfilled device.
    #[serde(default, alias = "deviceUuid")]
    pub device_uuid: Option<Uuid>,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ConfigSyncItem {
    pub id: Uuid,
    #[serde(default, alias = "deviceUuid")]
    pub device_uuid: Option<Uuid>,
    pub key: String,
    pub value: String,
    #[serde(alias = "syncState", default = "default_sync_state")]
    pub sync_state: String,
    pub version: i32,
    #[serde(alias = "isDeleted")]
    pub is_deleted: bool,
    #[serde(alias = "lastModified")]
    pub last_modified: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DrawingSyncItem {
    pub id: Uuid,
    #[serde(alias = "userId")]
    pub user_id: Option<String>,
    #[serde(default, alias = "deviceUuid")]
    pub device_uuid: Option<Uuid>,
    #[serde(alias = "createdAt")]
    pub created_at: i64,
    pub data: serde_json::Value,
    #[serde(alias = "syncState", default = "default_sync_state")]
    pub sync_state: String,
    pub version: i32,
    #[serde(alias = "isDeleted")]
    pub is_deleted: bool,
    #[serde(alias = "lastModified")]
    pub last_modified: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SyncScope {
    #[default]
    All,
    Grocery,
    Todo,
    ScribbleBox,
    ScribbleKeep,
    ScribbleKeepCloud,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SyncRequest {
    pub last_synced_at: Option<DateTime<Utc>>,
    pub client_id: String,
    /// The tablet making this request. Absent from pre-device clients, which fall back to
    /// the account's backfilled device.
    #[serde(default, alias = "deviceUuid")]
    pub device_uuid: Option<Uuid>,
    /// Human-readable label for `device_uuid`, generated on the client. Used to register
    /// the device on its first sync and to keep the stored name current.
    #[serde(default, alias = "deviceName")]
    pub device_name: Option<String>,
    #[serde(default)]
    pub scope: Option<SyncScope>,
    #[serde(default, alias = "todoListChanges")]
    pub todo_list_changes: Vec<TodoListChangeDelta>,
    #[serde(default, alias = "todoChanges")]
    pub todo_changes: Vec<TodoChangeDelta>,
    #[serde(default, alias = "groceryListChanges")]
    pub grocery_list_changes: Vec<GroceryListChangeDelta>,
    #[serde(default, alias = "groceryListMemberChanges")]
    pub grocery_list_member_changes: Vec<GroceryListMemberChangeDelta>,
    #[serde(default, alias = "storeChanges")]
    pub store_changes: Vec<StoreChangeDelta>,
    #[serde(default, alias = "categoryChanges")]
    pub category_changes: Vec<CategoryChangeDelta>,
    #[serde(default, alias = "groceryChanges")]
    pub grocery_changes: Vec<GroceryChangeDelta>,
    #[serde(default, alias = "groceryItemStoreInfoChanges")]
    pub grocery_item_store_info_changes: Vec<GroceryItemStoreInfoChangeDelta>,
    #[serde(default, alias = "configChanges")]
    pub config_changes: Vec<ConfigChangeDelta>,
    #[serde(default, alias = "drawingChanges")]
    pub drawing_changes: Vec<DrawingChangeDelta>,
    #[serde(default)]
    pub configs: Vec<ConfigSyncItem>,
    #[serde(default)]
    pub drawings: Vec<DrawingSyncItem>,
    /// Whether this client knows how to resume a download that was cut short.
    ///
    /// The download page bound in `crate::routes::sync::paging` is only safe for a client
    /// that carries its cursor forward on `last_synced_at` and comes back for the rest. A
    /// client that does not is served whole, exactly as before paging existed, because a
    /// page it can never ask past is not a bound — it is data it will never see again.
    ///
    /// This cannot be inferred from `last_synced_at` being present: a client that pages
    /// perfectly well still sends no cursor on its very first sync, which is precisely the
    /// request most in need of a bound. So it is asked for explicitly, and defaults to
    /// `false` for every client that shipped before the flag existed.
    #[serde(default, alias = "supportsPaging")]
    pub supports_paging: bool,
}

/// One request body, three readers.
///
/// `sync_handler` fans out into three concurrent futures (todo, grocery, config/drawing) and
/// each of them needs the whole request: the client id, `last_synced_at`, and its own slice
/// of the change vectors. Each future used to take its own `payload.clone()`, which deep-copies
/// the entire `SyncRequest` — including `drawings`, whose vector data is the bulk of the body
/// and can run to megabytes under the request body limit. Three futures meant three full
/// copies of that on every sync.
///
/// Nothing writes to the request, so one allocation shared behind an `Arc` serves all three.
/// This is a memory change only: the futures are still built the same way and still handed to
/// the same `try_join!`, so concurrency is untouched.
pub struct SharedRequest(std::sync::Arc<SyncRequest>);

impl SharedRequest {
    pub fn new(payload: SyncRequest) -> Self {
        Self(std::sync::Arc::new(payload))
    }

    /// A handle for one future to move into itself. Bumps a refcount; copies no payload.
    pub fn handle(&self) -> std::sync::Arc<SyncRequest> {
        std::sync::Arc::clone(&self.0)
    }
}

impl std::ops::Deref for SharedRequest {
    type Target = SyncRequest;

    fn deref(&self) -> &SyncRequest {
        &self.0
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SuccessResult {
    pub id: String,
    pub version: i32,
    pub sync_state: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SyncResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub success_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upload_status: Vec<SuccessResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_todo_list_changes: Vec<TodoListChangeDelta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_todo_changes: Vec<TodoChangeDelta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_grocery_list_changes: Vec<GroceryListChangeDelta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_grocery_list_member_changes: Vec<GroceryListMemberChangeDelta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_store_changes: Vec<StoreChangeDelta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_category_changes: Vec<CategoryChangeDelta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_grocery_changes: Vec<GroceryChangeDelta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_grocery_item_store_info_changes: Vec<GroceryItemStoreInfoChangeDelta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_config_changes: Vec<ConfigChangeDelta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_drawing_changes: Vec<DrawingChangeDelta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub configs: Vec<ConfigSyncItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drawings: Vec<DrawingSyncItem>,
    pub server_timestamp: DateTime<Utc>,
    /// Set when a download was cut short at a page boundary and the client still has rows
    /// waiting for it. Additive and omitted when false, so nothing shipped today sees it.
    ///
    /// A client that ignores it is still correct, because `server_timestamp` carries the
    /// truncation: it is walked back to the last millisecond this reply delivered in full,
    /// so the ordinary "store it and send it back as `last_synced_at`" loop picks the rest
    /// up on the next sync rather than skipping it. The flag only says "and you can do
    /// that immediately, rather than on your next scheduled poll". See
    /// `crate::routes::sync::paging`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub has_more: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug)]
pub enum AppError {
    Database(sqlx::Error),
    Serialization(serde_json::Error),
    Deserialization(String),
    /// A syntactically valid payload the server refuses on its own rules.
    BadRequest(String),
    /// The request is well formed and the caller is entitled to make it, but the row it
    /// targets cannot accept it in its current state — see
    /// [`crate::routes::sync::versioning::next_version`], where a row at the version
    /// ceiling ends up. Distinct from `BadRequest`: there is nothing to fix in the
    /// payload, and 409 is the status clients already read as "re-read and retry".
    Conflict(String),
    Gemini(String),
    Forbidden(String),
    NotFound(String),
    Redis(redis::RedisError),
    /// The caller is over a per-account concurrency limit. Distinct from
    /// `Forbidden`: nothing about the request is wrong, there is just already
    /// too much of it in flight, and it will succeed again once something closes.
    TooManyRequests(String),
    /// This replica is at capacity for a resource shared by every account. The
    /// request is fine and another replica — or this one, shortly — can serve it.
    Overloaded(String),
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let (status, error_message) = match self {
            // A pool acquire that timed out is not a bug in the request — the
            // service ran out of connections, or the database did not answer at
            // all (see `db_health`: an unreachable Neon surfaces as `PoolTimedOut`
            // too, because the pool absorbs the failed connects). Both are "come
            // back later", so both get a 503 and its retry semantics rather than a
            // 500 that tells a client its payload was at fault. This is what makes
            // the short `acquire_timeout` in `crate::db` shed load: fail fast, and
            // say so in a status code clients already back off on.
            AppError::Database(err @ sqlx::Error::PoolTimedOut) => {
                tracing::error!("Database pool exhausted or unreachable: {:?}", err);
                (StatusCode::SERVICE_UNAVAILABLE, "Database unavailable".to_string())
            }
            AppError::Database(err) => {
                tracing::error!("Database error: {:?}", err);
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal database error".to_string())
            }
            AppError::Serialization(err) => {
                tracing::error!("Serialization error: {:?}", err);
                (StatusCode::BAD_REQUEST, format!("Invalid payload: {}", err))
            }
            AppError::Deserialization(err) => {
                // Client-caused 400, for the same reason as the `json_rejected` line
                // in `AppJson::from_request` — which already logged this exact
                // rejection one frame earlier. Nothing here is the server's fault, so
                // it does not belong at the level operators alert on.
                tracing::warn!("Deserialization error: {}", err);
                (StatusCode::BAD_REQUEST, format!("Invalid JSON payload: {}", err))
            }
            AppError::BadRequest(err) => {
                tracing::warn!("Bad request: {}", err);
                (StatusCode::BAD_REQUEST, err)
            }
            AppError::Conflict(err) => {
                // Client-caused and self-explanatory, so `warn` rather than `error`: no
                // operator action follows, and the caller is told what to do.
                tracing::warn!("Conflict: {}", err);
                (StatusCode::CONFLICT, err)
            }
            AppError::Gemini(err) => {
                tracing::error!("Gemini error: {}", err);
                (StatusCode::SERVICE_UNAVAILABLE, "AI service error".to_string())
            }
            AppError::Forbidden(err) => {
                tracing::error!("Forbidden error: {}", err);
                (StatusCode::FORBIDDEN, err)
            }
            AppError::NotFound(err) => {
                tracing::warn!("Not found: {}", err);
                (StatusCode::NOT_FOUND, err)
            }
            AppError::Redis(err) => {
                tracing::error!("Redis error: {:?}", err);
                (StatusCode::INTERNAL_SERVER_ERROR, "Redis pubsub error".to_string())
            }
            AppError::TooManyRequests(err) => {
                tracing::warn!("Rejected, per-account limit: {}", err);
                (StatusCode::TOO_MANY_REQUESTS, err)
            }
            AppError::Overloaded(err) => {
                tracing::warn!("Rejected, replica at capacity: {}", err);
                (StatusCode::SERVICE_UNAVAILABLE, err)
            }
            AppError::Internal(err) => {
                tracing::error!("Internal error: {}", err);
                (StatusCode::INTERNAL_SERVER_ERROR, err)
            }
        };

        (
            status,
            axum::Json(serde_json::json!({ "error": error_message })),
        )
            .into_response()
    }
}

impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        // Every `?` on a database error in this service converts here, which
        // makes this the one place that sees all of them. Readiness cannot probe
        // Postgres without paying a Neon wake-up per probe, so this is where the
        // health signal comes from instead: real traffic, no extra queries.
        crate::observability::db_health::record_error(&err);
        AppError::Database(err)
    }
}

impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        AppError::Serialization(err)
    }
}

impl From<redis::RedisError> for AppError {
    fn from(err: redis::RedisError) -> Self {
        AppError::Redis(err)
    }
}


pub struct AppJson<T>(pub T);

#[axum::async_trait]
impl<S, T> axum::extract::FromRequest<S> for AppJson<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request(req: axum::http::Request<axum::body::Body>, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = axum::body::Bytes::from_request(req, state)
            .await
            .map_err(|rejection| {
                let err_msg = rejection.to_string();
                tracing::error!("Failed to read request body bytes: {}", err_msg);
                AppError::Deserialization(err_msg)
            })?;

        match serde_json::from_slice::<T>(&bytes) {
            Ok(value) => Ok(AppJson(value)),
            Err(err) => {
                let err_msg = truncate_parse_error(&err.to_string());
                let digest = describe_rejected_body(&bytes, &log_hash_salt_from_env());
                // Deliberately `warn`, not `error`. This branch is reached only when a
                // client sends something the server cannot parse: the service is
                // healthy, no operator action follows, and the caller already gets a
                // 400 saying so. It is also triggerable by anyone who can reach the
                // route, and `error` is the level that pages people — leaving it there
                // hands a stranger the alerting channel.
                tracing::warn!(
                    event = "json_rejected",
                    body_bytes = digest.len,
                    body_hash = %digest.hash,
                    error = %err_msg,
                    "request body failed to deserialize"
                );
                Err(AppError::Deserialization(err_msg))
            }
        }
    }
}

/// What is safe to say in a log line about a body that would not parse.
///
/// Never the bytes. `len` is the volume — which is what distinguishes a truncated
/// upload from a client sending nonsense — and `hash` is
/// [`crate::observability::http::hash_log_body`]: stable per distinct payload, so a
/// client stuck retrying one bad request collapses to a single recognisable digest,
/// and useless to anyone holding the logs who wants to know what the child drew.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedBodyDigest {
    pub len: usize,
    pub hash: String,
}

/// Split out from the extractor purely so the "what may be logged" decision is a
/// pure function a test can hold to account without standing up a subscriber.
pub fn describe_rejected_body(bytes: &[u8], salt: &str) -> RejectedBodyDigest {
    RejectedBodyDigest {
        len: bytes.len(),
        hash: crate::observability::http::hash_log_body(bytes, salt),
    }
}

/// Upper bound on how much of a serde error message reaches the log.
///
/// A serde_json message is *usually* structural: it names the field, the expected
/// type and the line/column, and quotes nothing — `missing field \`client_id\` at
/// line 3 column 5`. Usually is not always. A type mismatch on a string renders as
/// `invalid type: string "<the value>", expected i32`, which puts a fragment of the
/// caller's own data into the message. Bounding the length caps both that
/// disclosure and the log volume one oversized field can drive. A message long
/// enough to be cut has lost its trailing `at line L column C`, which is an
/// acceptable trade: that only happens for the pathological payloads whose contents
/// are precisely what we are trying not to write down.
const MAX_PARSE_ERROR_LEN: usize = 200;

/// Truncates on a char boundary, so a multi-byte character is never split in half.
pub fn truncate_parse_error(message: &str) -> String {
    if message.len() <= MAX_PARSE_ERROR_LEN {
        return message.to_string();
    }
    let mut end = MAX_PARSE_ERROR_LEN;
    while end > 0 && !message.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…(truncated)", &message[..end])
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TodoListData {
    pub id: String,
    pub name: String,
    #[serde(alias = "color_hex")]
    pub color_hex: String,
    #[serde(alias = "user_id")]
    pub user_id: Option<String>,
    #[serde(alias = "created_at")]
    pub created_at: i64,
    #[serde(alias = "sync_state")]
    pub sync_state: String,
    pub version: i32,
    #[serde(alias = "is_deleted")]
    pub is_deleted: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TodoItemData {
    pub id: String,
    pub title: String,
    #[serde(alias = "is_completed")]
    pub is_completed: bool,
    #[serde(alias = "created_at")]
    pub created_at: i64,
    pub position: i32,
    #[serde(alias = "scheduled_date")]
    pub scheduled_date: Option<String>,
    #[serde(alias = "recurrence_rule")]
    pub recurrence_rule: Option<String>,
    #[serde(alias = "scheduled_at")]
    pub scheduled_at: i64,
    #[serde(alias = "user_id")]
    pub user_id: Option<String>,
    #[serde(alias = "parent_id")]
    pub parent_id: Option<String>,
    #[serde(alias = "is_daily")]
    pub is_daily: bool,
    #[serde(alias = "due_date")]
    pub due_date: Option<i64>,
    pub description: Option<String>,
    #[serde(alias = "list_id")]
    pub list_id: Option<String>,
    pub priority: i32,
    pub icon: Option<String>,
    #[serde(alias = "sync_state")]
    pub sync_state: String,
    pub version: i32,
    #[serde(alias = "is_deleted")]
    pub is_deleted: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroceryListData {
    pub id: String,
    pub name: String,
    #[serde(alias = "owner_id")]
    pub owner_id: Option<String>,
    #[serde(alias = "created_at")]
    pub created_at: i64,
    #[serde(alias = "sync_state", default = "default_sync_state")]
    pub sync_state: String,
    pub version: i32,
    #[serde(alias = "is_deleted", default)]
    pub is_deleted: bool,
}

fn default_sync_state() -> String {
    "SYNCED".to_string()
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroceryListMemberData {
    pub id: String,
    #[serde(alias = "list_id")]
    pub list_id: String,
    #[serde(alias = "user_id")]
    pub user_id: String,
    pub role: String,
    #[serde(alias = "joined_at")]
    pub joined_at: i64,
    #[serde(alias = "sync_state", default = "default_sync_state")]
    pub sync_state: String,
    pub version: i32,
    #[serde(alias = "is_deleted", default)]
    pub is_deleted: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StoreData {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub position: i32,
    #[serde(alias = "is_default_supported")]
    pub is_default_supported: bool,
    #[serde(alias = "user_id")]
    pub user_id: Option<String>,
    #[serde(alias = "sync_state", default = "default_sync_state")]
    pub sync_state: String,
    pub version: i32,
    #[serde(alias = "is_deleted", default)]
    pub is_deleted: bool,
    #[serde(alias = "listId", alias = "list_id")]
    pub list_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CategoryData {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub position: i32,
    #[serde(alias = "user_id")]
    pub user_id: Option<String>,
    pub icon: Option<String>,
    #[serde(alias = "sync_state", default = "default_sync_state")]
    pub sync_state: String,
    pub version: i32,
    #[serde(alias = "is_deleted", default)]
    pub is_deleted: bool,
    #[serde(alias = "listId", alias = "list_id")]
    pub list_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroceryItemData {
    pub id: String,
    pub name: String,
    pub quantity: String,
    #[serde(alias = "is_bought")]
    pub is_bought: bool,
    #[serde(alias = "created_at")]
    pub created_at: i64,
    pub position: i32,
    #[serde(alias = "category_id", default)]
    pub category_id: Option<String>,
    #[serde(alias = "times_bought")]
    pub times_bought: i32,
    #[serde(alias = "user_id")]
    pub user_id: Option<String>,
    #[serde(alias = "is_active")]
    pub is_active: bool,
    #[serde(alias = "list_id")]
    pub list_id: Option<String>,
    pub unit: Option<String>,
    pub notes: Option<String>,
    #[serde(alias = "sync_state", default = "default_sync_state")]
    pub sync_state: String,
    pub version: i32,
    pub is_deleted: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroceryItemStoreInfoData {
    #[serde(default)]
    pub id: String,
    #[serde(alias = "groceryItemId")]
    pub grocery_item_id: String,
    #[serde(alias = "storeId")]
    pub store_id: String,
    /// Server-computed convenience field. Not a stored column: there is no `listId` on
    /// `grocery_item_store_info`, and the list a row belongs to is always resolved from its
    /// parent (the store on the download path, the grocery item on the echo path — the same
    /// list, given an item and its stores share one). Populated on the way out; ignored on
    /// the way in, so a client can round-trip it without ever making it authoritative.
    #[serde(default, alias = "listId")]
    pub list_id: Option<String>,
    pub price: Option<f64>,
    #[serde(alias = "isAvailable")]
    pub is_available: bool,
    #[serde(alias = "userId")]
    pub user_id: Option<String>,
    #[serde(alias = "syncState", default = "default_sync_state")]
    pub sync_state: String,
    pub version: i32,
    #[serde(alias = "isDeleted", default)]
    pub is_deleted: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ConfigData {
    pub id: Uuid,
    #[serde(alias = "userId")]
    pub user_id: String,
    #[serde(alias = "clientUuid")]
    pub client_uuid: String,
    #[serde(default, alias = "deviceUuid")]
    pub device_uuid: Option<Uuid>,
    pub version: i32,
    #[serde(alias = "isDeleted")]
    pub is_deleted: bool,
    #[serde(alias = "lastModified")]
    pub last_modified: i64,
    #[serde(alias = "syncState", default = "default_sync_state")]
    pub sync_state: String,
    pub key: String,
    pub value: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DrawingData {
    pub id: Uuid,
    #[serde(alias = "userId")]
    pub user_id: String,
    #[serde(alias = "clientUuid")]
    pub client_uuid: String,
    #[serde(default, alias = "deviceUuid")]
    pub device_uuid: Option<Uuid>,
    pub version: i32,
    #[serde(alias = "isDeleted")]
    pub is_deleted: bool,
    #[serde(alias = "lastModified")]
    pub last_modified: i64,
    #[serde(alias = "syncState", default = "default_sync_state")]
    pub sync_state: String,
    #[serde(alias = "createdAt")]
    pub created_at: i64,
    pub data: serde_json::Value,
}
