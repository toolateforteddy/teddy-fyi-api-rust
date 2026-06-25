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
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DrawingChangeDelta {
    pub id: String,
    #[serde(rename = "type")]
    pub operation_type: OperationType,
    pub version: i32,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ConfigSyncItem {
    pub id: Uuid,
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

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SyncScope {
    All,
    Grocery,
    Todo,
    ScribbleBox,
    ScribbleKeep,
    ScribbleKeepCloud,
}

impl Default for SyncScope {
    fn default() -> Self {
        Self::All
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SyncRequest {
    pub last_synced_at: Option<DateTime<Utc>>,
    pub client_id: String,
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
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SuccessResult {
    pub id: String,
    pub version: i32,
    pub sync_state: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SyncResponse {
    pub success_ids: Vec<String>,
    pub upload_status: Vec<SuccessResult>,
    pub remote_todo_list_changes: Vec<TodoListChangeDelta>,
    pub remote_todo_changes: Vec<TodoChangeDelta>,
    pub remote_grocery_list_changes: Vec<GroceryListChangeDelta>,
    pub remote_grocery_list_member_changes: Vec<GroceryListMemberChangeDelta>,
    pub remote_store_changes: Vec<StoreChangeDelta>,
    pub remote_category_changes: Vec<CategoryChangeDelta>,
    pub remote_grocery_changes: Vec<GroceryChangeDelta>,
    pub remote_grocery_item_store_info_changes: Vec<GroceryItemStoreInfoChangeDelta>,
    pub remote_config_changes: Vec<ConfigChangeDelta>,
    pub remote_drawing_changes: Vec<DrawingChangeDelta>,
    #[serde(default)]
    pub configs: Vec<ConfigSyncItem>,
    #[serde(default)]
    pub drawings: Vec<DrawingSyncItem>,
    pub server_timestamp: DateTime<Utc>,
}

#[derive(Debug)]
pub enum AppError {
    Database(sqlx::Error),
    Serialization(serde_json::Error),
    Deserialization(String),
    Gemini(String),
    Forbidden(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let (status, error_message) = match self {
            AppError::Database(err) => {
                tracing::error!("Database error: {:?}", err);
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal database error".to_string())
            }
            AppError::Serialization(err) => {
                tracing::error!("Serialization error: {:?}", err);
                (StatusCode::BAD_REQUEST, format!("Invalid payload: {}", err))
            }
            AppError::Deserialization(err) => {
                tracing::error!("Deserialization error: {}", err);
                (StatusCode::BAD_REQUEST, format!("Invalid JSON payload: {}", err))
            }
            AppError::Gemini(err) => {
                tracing::error!("Gemini error: {}", err);
                (StatusCode::SERVICE_UNAVAILABLE, "AI service error".to_string())
            }
            AppError::Forbidden(err) => {
                tracing::error!("Forbidden error: {}", err);
                (StatusCode::FORBIDDEN, err)
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
        AppError::Database(err)
    }
}

impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        AppError::Serialization(err)
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
                let err_msg = err.to_string();
                let body_str = String::from_utf8_lossy(&bytes);
                tracing::error!("JSON deserialization rejection. Error: {}. Body: {}", err_msg, body_str);
                Err(AppError::Deserialization(err_msg))
            }
        }
    }
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
