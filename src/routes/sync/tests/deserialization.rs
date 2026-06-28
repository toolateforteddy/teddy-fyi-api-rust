use crate::routes::sync::{SyncRequest, SyncScope};

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
