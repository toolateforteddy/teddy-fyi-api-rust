use axum::{http::StatusCode, response::IntoResponse};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, Deserializer};

pub fn deserialize_i32_from_string_or_number<'de, D>(deserializer: D) -> Result<i32, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrNumber {
        String(String),
        Number(i32),
    }

    match StringOrNumber::deserialize(deserializer)? {
        StringOrNumber::String(s) => s.parse::<i32>().map_err(serde::de::Error::custom),
        StringOrNumber::Number(n) => Ok(n),
    }
}

pub fn deserialize_option_i32_from_string_or_number<'de, D>(deserializer: D) -> Result<Option<i32>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrNumber {
        String(String),
        Number(i32),
        Null,
    }

    match Option::<StringOrNumber>::deserialize(deserializer)? {
        Some(StringOrNumber::String(s)) => {
            if s.is_empty() || s == "null" {
                Ok(None)
            } else {
                s.parse::<i32>().map(Some).map_err(serde::de::Error::custom)
            }
        }
        Some(StringOrNumber::Number(n)) => Ok(Some(n)),
        Some(StringOrNumber::Null) | None => Ok(None),
    }
}

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
    #[serde(deserialize_with = "deserialize_i32_from_string_or_number")]
    pub id: i32,
    #[serde(rename = "type")]
    pub operation_type: OperationType,
    pub version: i32,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CategoryChangeDelta {
    #[serde(deserialize_with = "deserialize_i32_from_string_or_number")]
    pub id: i32,
    #[serde(rename = "type")]
    pub operation_type: OperationType,
    pub version: i32,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroceryChangeDelta {
    #[serde(deserialize_with = "deserialize_i32_from_string_or_number")]
    pub id: i32,
    #[serde(rename = "type")]
    pub operation_type: OperationType,
    pub version: i32,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroceryItemStoreInfoChangeDelta {
    #[serde(alias = "grocery_item_id", rename = "groceryItemId", deserialize_with = "deserialize_i32_from_string_or_number")]
    pub grocery_item_id: i32,
    #[serde(alias = "store_id", rename = "storeId", deserialize_with = "deserialize_i32_from_string_or_number")]
    pub store_id: i32,
    #[serde(rename = "type")]
    pub operation_type: OperationType,
    pub version: i32,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SyncScope {
    All,
    Grocery,
    Todo,
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
        match axum::Json::<T>::from_request(req, state).await {
            Ok(value) => Ok(AppJson(value.0)),
            Err(rejection) => {
                let err_msg = rejection.to_string();
                tracing::error!("JSON deserialization rejection: {}", err_msg);
                Err(AppError::Deserialization(err_msg))
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TodoListData {
    pub id: String,
    pub name: String,
    #[serde(alias = "color_hex", rename = "colorHex")]
    pub color_hex: String,
    #[serde(alias = "user_id", rename = "userId")]
    pub user_id: Option<String>,
    #[serde(alias = "created_at", rename = "createdAt")]
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
    #[serde(alias = "is_completed", rename = "isCompleted")]
    pub is_completed: bool,
    #[serde(alias = "created_at", rename = "createdAt")]
    pub created_at: i64,
    pub position: i32,
    #[serde(alias = "scheduled_date", rename = "scheduledDate")]
    pub scheduled_date: Option<String>,
    #[serde(alias = "recurrence_rule", rename = "recurrenceRule")]
    pub recurrence_rule: Option<String>,
    #[serde(alias = "scheduled_at", rename = "scheduledAt")]
    pub scheduled_at: i64,
    #[serde(alias = "user_id", rename = "userId")]
    pub user_id: Option<String>,
    #[serde(alias = "parent_id", rename = "parentId")]
    pub parent_id: Option<String>,
    #[serde(alias = "is_daily", rename = "isDaily")]
    pub is_daily: bool,
    #[serde(alias = "due_date", rename = "dueDate")]
    pub due_date: Option<i64>,
    pub description: Option<String>,
    #[serde(alias = "list_id", rename = "listId")]
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
    #[serde(alias = "owner_id", rename = "ownerId")]
    pub owner_id: Option<String>,
    #[serde(alias = "created_at", rename = "createdAt")]
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
    #[serde(alias = "list_id", rename = "listId")]
    pub list_id: String,
    #[serde(alias = "user_id", rename = "userId")]
    pub user_id: String,
    pub role: String,
    #[serde(alias = "joined_at", rename = "joinedAt")]
    pub joined_at: i64,
    #[serde(alias = "sync_state", default = "default_sync_state")]
    pub sync_state: String,
    pub version: i32,
    #[serde(alias = "is_deleted", default)]
    pub is_deleted: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StoreData {
    #[serde(deserialize_with = "deserialize_i32_from_string_or_number")]
    pub id: i32,
    pub name: String,
    #[serde(default)]
    pub position: i32,
    #[serde(alias = "is_default_supported", rename = "isDefaultSupported")]
    pub is_default_supported: bool,
    #[serde(alias = "user_id", rename = "userId")]
    pub user_id: Option<String>,
    #[serde(alias = "sync_state", default = "default_sync_state")]
    pub sync_state: String,
    pub version: i32,
    #[serde(alias = "is_deleted", default)]
    pub is_deleted: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CategoryData {
    #[serde(deserialize_with = "deserialize_i32_from_string_or_number")]
    pub id: i32,
    pub name: String,
    #[serde(default)]
    pub position: i32,
    #[serde(alias = "user_id", rename = "userId")]
    pub user_id: Option<String>,
    pub icon: Option<String>,
    #[serde(alias = "sync_state", default = "default_sync_state")]
    pub sync_state: String,
    pub version: i32,
    #[serde(alias = "is_deleted", default)]
    pub is_deleted: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroceryItemData {
    #[serde(deserialize_with = "deserialize_i32_from_string_or_number")]
    pub id: i32,
    pub name: String,
    pub quantity: String,
    #[serde(alias = "is_bought", rename = "isBought")]
    pub is_bought: bool,
    #[serde(alias = "created_at", rename = "createdAt")]
    pub created_at: i64,
    pub position: i32,
    #[serde(alias = "category_id", rename = "categoryId", default, deserialize_with = "deserialize_option_i32_from_string_or_number")]
    pub category_id: Option<i32>,
    #[serde(alias = "times_bought", rename = "timesBought")]
    pub times_bought: i32,
    #[serde(alias = "user_id", rename = "userId")]
    pub user_id: Option<String>,
    #[serde(alias = "is_active", rename = "isActive")]
    pub is_active: bool,
    #[serde(alias = "list_id", rename = "listId")]
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
    #[serde(alias = "grocery_item_id", rename = "groceryItemId", deserialize_with = "deserialize_i32_from_string_or_number")]
    pub grocery_item_id: i32,
    #[serde(alias = "store_id", rename = "storeId", deserialize_with = "deserialize_i32_from_string_or_number")]
    pub store_id: i32,
    pub price: Option<f64>,
    #[serde(alias = "is_available", rename = "isAvailable")]
    pub is_available: bool,
    #[serde(alias = "user_id", rename = "userId")]
    pub user_id: Option<String>,
    #[serde(alias = "sync_state", default = "default_sync_state")]
    pub sync_state: String,
    pub version: i32,
    #[serde(alias = "is_deleted", default)]
    pub is_deleted: bool,
}
