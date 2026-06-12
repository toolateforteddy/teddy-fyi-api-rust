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
    #[serde(rename = "groceryItemId", deserialize_with = "deserialize_i32_from_string_or_number")]
    pub grocery_item_id: i32,
    #[serde(rename = "storeId", deserialize_with = "deserialize_i32_from_string_or_number")]
    pub store_id: i32,
    #[serde(rename = "type")]
    pub operation_type: OperationType,
    pub version: i32,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SyncRequest {
    pub last_synced_at: Option<DateTime<Utc>>,
    pub client_id: String,
    #[serde(default)]
    pub todo_list_changes: Vec<TodoListChangeDelta>,
    #[serde(default)]
    pub todo_changes: Vec<TodoChangeDelta>,
    #[serde(default)]
    pub grocery_list_changes: Vec<GroceryListChangeDelta>,
    #[serde(default)]
    pub grocery_list_member_changes: Vec<GroceryListMemberChangeDelta>,
    #[serde(default)]
    pub store_changes: Vec<StoreChangeDelta>,
    #[serde(default)]
    pub category_changes: Vec<CategoryChangeDelta>,
    #[serde(default)]
    pub grocery_changes: Vec<GroceryChangeDelta>,
    #[serde(default)]
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
    Gemini(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let (status, error_message) = match self {
            AppError::Database(err) => {
                tracing::error!("Database error: {:?}", err);
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal database error")
            }
            AppError::Serialization(err) => {
                tracing::error!("Serialization error: {:?}", err);
                (StatusCode::BAD_REQUEST, "Invalid payload")
            }
            AppError::Gemini(err) => {
                tracing::error!("Gemini error: {}", err);
                (StatusCode::SERVICE_UNAVAILABLE, "AI service error")
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TodoListData {
    pub id: String,
    pub name: String,
    #[serde(rename = "colorHex")]
    pub color_hex: String,
    #[serde(rename = "userId")]
    pub user_id: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    pub sync_state: String,
    pub version: i32,
    pub is_deleted: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TodoItemData {
    pub id: String,
    pub title: String,
    #[serde(rename = "isCompleted")]
    pub is_completed: bool,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    pub position: i32,
    #[serde(rename = "scheduledDate")]
    pub scheduled_date: Option<String>,
    #[serde(rename = "recurrenceRule")]
    pub recurrence_rule: Option<String>,
    #[serde(rename = "scheduledAt")]
    pub scheduled_at: i64,
    #[serde(rename = "userId")]
    pub user_id: Option<String>,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    #[serde(rename = "isDaily")]
    pub is_daily: bool,
    #[serde(rename = "dueDate")]
    pub due_date: Option<i64>,
    pub description: Option<String>,
    #[serde(rename = "listId")]
    pub list_id: Option<String>,
    pub priority: i32,
    pub icon: Option<String>,
    pub sync_state: String,
    pub version: i32,
    pub is_deleted: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroceryListData {
    pub id: String,
    pub name: String,
    #[serde(rename = "ownerId")]
    pub owner_id: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    pub version: i32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroceryListMemberData {
    pub id: String,
    #[serde(rename = "listId")]
    pub list_id: String,
    #[serde(rename = "userId")]
    pub user_id: String,
    pub role: String,
    #[serde(rename = "joinedAt")]
    pub joined_at: i64,
    pub version: i32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StoreData {
    #[serde(deserialize_with = "deserialize_i32_from_string_or_number")]
    pub id: i32,
    pub name: String,
    #[serde(default)]
    pub position: i32,
    #[serde(default, rename = "isDefaultSupported")]
    pub is_default_supported: bool,
    #[serde(default, rename = "userId")]
    pub user_id: Option<String>,
    pub version: i32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CategoryData {
    #[serde(deserialize_with = "deserialize_i32_from_string_or_number")]
    pub id: i32,
    pub name: String,
    #[serde(default)]
    pub position: i32,
    #[serde(default, rename = "userId")]
    pub user_id: Option<String>,
    pub icon: Option<String>,
    pub version: i32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroceryItemData {
    #[serde(deserialize_with = "deserialize_i32_from_string_or_number")]
    pub id: i32,
    pub name: String,
    pub quantity: String,
    #[serde(rename = "isBought")]
    pub is_bought: bool,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    pub position: i32,
    #[serde(default, rename = "categoryId", deserialize_with = "deserialize_option_i32_from_string_or_number")]
    pub category_id: Option<i32>,
    #[serde(rename = "timesBought")]
    pub times_bought: i32,
    #[serde(rename = "userId")]
    pub user_id: Option<String>,
    #[serde(rename = "isActive")]
    pub is_active: bool,
    #[serde(rename = "listId")]
    pub list_id: Option<String>,
    pub unit: Option<String>,
    pub notes: Option<String>,
    pub version: i32,
    pub is_deleted: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroceryItemStoreInfoData {
    #[serde(rename = "groceryItemId", deserialize_with = "deserialize_i32_from_string_or_number")]
    pub grocery_item_id: i32,
    #[serde(rename = "storeId", deserialize_with = "deserialize_i32_from_string_or_number")]
    pub store_id: i32,
    pub price: Option<f64>,
    #[serde(rename = "isAvailable")]
    pub is_available: bool,
    #[serde(rename = "userId")]
    pub user_id: Option<String>,
    pub version: i32,
}
