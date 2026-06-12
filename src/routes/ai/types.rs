use serde::{Deserialize, Serialize};
use schemars::JsonSchema;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CategorizeItemRequest {
    pub item_title: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CategorizeItemResponse {
    pub selected_category: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AssignTodoIconRequest {
    pub todo_title: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AssignTodoIconResponse {
    pub emoji_or_asset_token: String,
}
