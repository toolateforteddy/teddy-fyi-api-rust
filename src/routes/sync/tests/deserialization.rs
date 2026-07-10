use crate::routes::sync::{SyncRequest, SyncScope, SyncResponse};

#[test]
fn test_sync_request_deserialization_defaults() {
    let json_data = r#"{
        "client_id": "test-client-id"
    }"#;

    let req: SyncRequest = serde_json::from_str(json_data).expect("Should deserialize successfully with missing fields");
    assert_eq!(req.client_id, "test-client-id");
    assert!(req.last_synced_at.is_none());
    assert!(req.scope.is_none());
    assert!(req.todo_list_changes.is_empty());
    assert!(req.todo_changes.is_empty());
    assert!(req.grocery_list_changes.is_empty());
    assert!(req.grocery_list_member_changes.is_empty());
    assert!(req.store_changes.is_empty());
    assert!(req.category_changes.is_empty());
    assert!(req.grocery_changes.is_empty());
    assert!(req.grocery_item_store_info_changes.is_empty());
}

#[test]
fn test_sync_request_deserialization_null_scope() {
    let json_data = r#"{
        "client_id": "test-client-id",
        "scope": null
    }"#;

    let req: SyncRequest = serde_json::from_str(json_data).expect("Should deserialize successfully with null scope");
    assert!(req.scope.is_none());
}

#[test]
fn test_sync_request_deserialization_scope_grocery() {
    let json_data = r#"{
        "client_id": "test-client-id",
        "scope": "GROCERY"
    }"#;

    let req: SyncRequest = serde_json::from_str(json_data).expect("Should deserialize successfully with GROCERY scope");
    assert_eq!(req.scope, Some(SyncScope::Grocery));
}

#[test]
fn test_sync_response_serialization_omits_empty_fields() {
    let now = chrono::Utc::now();
    let response = SyncResponse {
        success_ids: Vec::new(),
        upload_status: Vec::new(),
        remote_todo_list_changes: Vec::new(),
        remote_todo_changes: Vec::new(),
        remote_grocery_list_changes: Vec::new(),
        remote_grocery_list_member_changes: Vec::new(),
        remote_store_changes: Vec::new(),
        remote_category_changes: Vec::new(),
        remote_grocery_changes: Vec::new(),
        remote_grocery_item_store_info_changes: Vec::new(),
        remote_config_changes: Vec::new(),
        remote_drawing_changes: Vec::new(),
        configs: Vec::new(),
        drawings: Vec::new(),
        server_timestamp: now,
    };

    let serialized = serde_json::to_string(&response).expect("Should serialize successfully");
    
    // The JSON should only contain the server_timestamp
    let expected_timestamp = serde_json::to_string(&now).unwrap();
    let expected_json = format!("{{\"server_timestamp\":{}}}", expected_timestamp);
    assert_eq!(serialized, expected_json);

    // Deserializing this minimal JSON should yield the same struct with empty lists
    let deserialized: SyncResponse = serde_json::from_str(&serialized).expect("Should deserialize successfully");
    assert!(deserialized.success_ids.is_empty());
    assert!(deserialized.upload_status.is_empty());
    assert!(deserialized.remote_todo_list_changes.is_empty());
    assert!(deserialized.remote_todo_changes.is_empty());
    assert!(deserialized.remote_grocery_list_changes.is_empty());
    assert!(deserialized.remote_grocery_list_member_changes.is_empty());
    assert!(deserialized.remote_store_changes.is_empty());
    assert!(deserialized.remote_category_changes.is_empty());
    assert!(deserialized.remote_grocery_changes.is_empty());
    assert!(deserialized.remote_grocery_item_store_info_changes.is_empty());
    assert!(deserialized.remote_config_changes.is_empty());
    assert!(deserialized.remote_drawing_changes.is_empty());
    assert!(deserialized.configs.is_empty());
    assert!(deserialized.drawings.is_empty());
    assert_eq!(deserialized.server_timestamp, now);
}
