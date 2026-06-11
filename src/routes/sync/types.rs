use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use axum::{http::StatusCode, response::IntoResponse};

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
    pub id: i32,
    #[serde(rename = "type")]
    pub operation_type: OperationType,
    pub version: i32,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CategoryChangeDelta {
    pub id: i32,
    #[serde(rename = "type")]
    pub operation_type: OperationType,
    pub version: i32,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroceryChangeDelta {
    pub id: i32,
    #[serde(rename = "type")]
    pub operation_type: OperationType,
    pub version: i32,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroceryItemStoreInfoChangeDelta {
    #[serde(rename = "groceryItemId")]
    pub grocery_item_id: i32,
    #[serde(rename = "storeId")]
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
    pub todo_list_changes: Vec<TodoListChangeDelta>,
    pub todo_changes: Vec<TodoChangeDelta>,
    pub grocery_list_changes: Vec<GroceryListChangeDelta>,
    pub grocery_list_member_changes: Vec<GroceryListMemberChangeDelta>,
    pub store_changes: Vec<StoreChangeDelta>,
    pub category_changes: Vec<CategoryChangeDelta>,
    pub grocery_changes: Vec<GroceryChangeDelta>,
    pub grocery_item_store_info_changes: Vec<GroceryItemStoreInfoChangeDelta>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SyncResponse {
    pub success_ids: Vec<String>,
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
        };

        (status, axum::Json(serde_json::json!({ "error": error_message }))).into_response()
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
    pub id: i32,
    pub name: String,
    pub position: i32,
    #[serde(rename = "isDefaultSupported")]
    pub is_default_supported: bool,
    #[serde(rename = "userId")]
    pub user_id: Option<String>,
    pub version: i32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CategoryData {
    pub id: i32,
    pub name: String,
    pub position: i32,
    #[serde(rename = "userId")]
    pub user_id: Option<String>,
    pub version: i32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroceryItemData {
    pub id: i32,
    pub name: String,
    pub quantity: String,
    #[serde(rename = "isBought")]
    pub is_bought: bool,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    pub position: i32,
    #[serde(rename = "categoryId")]
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
    #[serde(rename = "groceryItemId")]
    pub grocery_item_id: i32,
    #[serde(rename = "storeId")]
    pub store_id: i32,
    pub price: Option<f64>,
    #[serde(rename = "isAvailable")]
    pub is_available: bool,
    #[serde(rename = "userId")]
    pub user_id: Option<String>,
    pub version: i32,
}
