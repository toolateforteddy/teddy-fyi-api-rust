use crate::routes::sync::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

pub async fn fetch_remote_grocery_mutations(
    tx: &mut Transaction<'_, Postgres>,
    client_id: &str,
    last_synced_at: Option<DateTime<Utc>>,
) -> Result<
    (
        Vec<GroceryListChangeDelta>,
        Vec<GroceryListMemberChangeDelta>,
        Vec<StoreChangeDelta>,
        Vec<CategoryChangeDelta>,
        Vec<GroceryChangeDelta>,
        Vec<GroceryItemStoreInfoChangeDelta>,
    ),
    AppError,
> {
    let mut remote_grocery_list_changes = Vec::new();
    let mut remote_grocery_list_member_changes = Vec::new();
    let mut remote_store_changes = Vec::new();
    let mut remote_category_changes = Vec::new();
    let mut remote_grocery_changes = Vec::new();
    let mut remote_grocery_item_store_info_changes = Vec::new();

    if let Some(last_synced_at) = last_synced_at {
        // Fetch grocery_lists changed after last_synced_at by OTHER clients
        let updated_lists = sqlx::query!(
            r#"SELECT id, name, "ownerId" as owner_id, "createdAt" as created_at, version, is_deleted, sync_state
               FROM grocery_lists
               WHERE updated_at > $1 AND (updated_by_client != $2 OR updated_by_client IS NULL)"#,
            last_synced_at,
            client_id
        )
        .fetch_all(&mut **tx)
        .await?;

        for row in updated_lists {
            let item_data = GroceryListData {
                id: row.id.clone(),
                name: row.name,
                owner_id: row.owner_id,
                created_at: row.created_at,
                version: row.version,
                is_deleted: row.is_deleted,
                sync_state: row.sync_state,
            };
            let data_val = serde_json::to_value(&item_data)?;
            remote_grocery_list_changes.push(GroceryListChangeDelta {
                id: row.id,
                operation_type: if row.is_deleted { OperationType::Delete } else { OperationType::Update },
                version: row.version,
                data: Some(data_val),
            });
        }

        // Fetch grocery_list_members changed after last_synced_at by OTHER clients
        let updated_members = sqlx::query!(
            r#"SELECT id, "listId" as list_id, "userId" as user_id, role, "joinedAt" as joined_at, version, is_deleted, sync_state
               FROM grocery_list_members
               WHERE updated_at > $1 AND (updated_by_client != $2 OR updated_by_client IS NULL)"#,
            last_synced_at,
            client_id
        )
        .fetch_all(&mut **tx)
        .await?;

        for row in updated_members {
            let item_data = GroceryListMemberData {
                id: row.id.clone(),
                list_id: row.list_id,
                user_id: row.user_id,
                role: row.role,
                joined_at: row.joined_at,
                version: row.version,
                is_deleted: row.is_deleted,
                sync_state: row.sync_state,
            };
            let data_val = serde_json::to_value(&item_data)?;
            remote_grocery_list_member_changes.push(GroceryListMemberChangeDelta {
                id: row.id,
                operation_type: if row.is_deleted { OperationType::Delete } else { OperationType::Update },
                version: row.version,
                data: Some(data_val),
            });
        }

        // Fetch stores changed after last_synced_at by OTHER clients
        let updated_stores = sqlx::query!(
            r#"SELECT id, name, position, "isDefaultSupported" as is_default_supported, "userId" as user_id, version, is_deleted, sync_state
               FROM stores
               WHERE updated_at > $1 AND (updated_by_client != $2 OR updated_by_client IS NULL)"#,
            last_synced_at,
            client_id
        )
        .fetch_all(&mut **tx)
        .await?;

        for row in updated_stores {
            let item_data = StoreData {
                id: row.id,
                name: row.name,
                position: row.position,
                is_default_supported: row.is_default_supported,
                user_id: row.user_id,
                version: row.version,
                is_deleted: row.is_deleted,
                sync_state: row.sync_state,
            };
            let data_val = serde_json::to_value(&item_data)?;
            remote_store_changes.push(StoreChangeDelta {
                id: row.id,
                operation_type: if row.is_deleted { OperationType::Delete } else { OperationType::Update },
                version: row.version,
                data: Some(data_val),
            });
        }

        // Fetch categories changed after last_synced_at by OTHER clients
        let updated_categories = sqlx::query!(
            r#"SELECT id, name, position, "userId" as user_id, icon, version, is_deleted, sync_state
               FROM categories
               WHERE updated_at > $1 AND (updated_by_client != $2 OR updated_by_client IS NULL)"#,
            last_synced_at,
            client_id
        )
        .fetch_all(&mut **tx)
        .await?;

        for row in updated_categories {
            let item_data = CategoryData {
                id: row.id,
                name: row.name,
                position: row.position,
                user_id: row.user_id,
                icon: row.icon,
                version: row.version,
                is_deleted: row.is_deleted,
                sync_state: row.sync_state,
            };
            let data_val = serde_json::to_value(&item_data)?;
            remote_category_changes.push(CategoryChangeDelta {
                id: row.id,
                operation_type: if row.is_deleted { OperationType::Delete } else { OperationType::Update },
                version: row.version,
                data: Some(data_val),
            });
        }

        // Fetch grocery_items changed after last_synced_at by OTHER clients
        let updated_groceries = sqlx::query!(
            r#"SELECT
                id, name, quantity, "isBought" as is_bought, "createdAt" as created_at, position, "categoryId" as category_id,
                "timesBought" as times_bought, "userId" as user_id, "isActive" as is_active, "listId" as list_id, unit, notes, version, is_deleted, sync_state
               FROM grocery_items
               WHERE updated_at > $1 AND (updated_by_client != $2 OR updated_by_client IS NULL)"#,
            last_synced_at,
            client_id
        )
        .fetch_all(&mut **tx)
        .await?;

        for row in updated_groceries {
            let item_data = GroceryItemData {
                id: row.id,
                name: row.name,
                quantity: row.quantity,
                is_bought: row.is_bought,
                created_at: row.created_at,
                position: row.position,
                category_id: row.category_id,
                times_bought: row.times_bought,
                user_id: row.user_id,
                is_active: row.is_active,
                list_id: row.list_id,
                unit: row.unit,
                notes: row.notes,
                version: row.version,
                is_deleted: row.is_deleted,
                sync_state: row.sync_state,
            };

            let data_val = serde_json::to_value(&item_data)?;

            remote_grocery_changes.push(GroceryChangeDelta {
                id: row.id,
                operation_type: if row.is_deleted {
                    OperationType::Delete
                } else {
                    OperationType::Update
                },
                version: row.version,
                data: Some(data_val),
            });
        }

        // Fetch grocery_item_store_info changed after last_synced_at by OTHER clients
        let updated_store_infos = sqlx::query!(
            r#"SELECT "groceryItemId" as grocery_item_id, "storeId" as store_id, price, "isAvailable" as is_available, "userId" as user_id, version, is_deleted, sync_state
               FROM grocery_item_store_info
               WHERE updated_at > $1 AND (updated_by_client != $2 OR updated_by_client IS NULL)"#,
            last_synced_at,
            client_id
        )
        .fetch_all(&mut **tx)
        .await?;

        for row in updated_store_infos {
            let item_data = GroceryItemStoreInfoData {
                grocery_item_id: row.grocery_item_id,
                store_id: row.store_id,
                price: row.price,
                is_available: row.is_available,
                user_id: row.user_id,
                version: row.version,
                is_deleted: row.is_deleted,
                sync_state: row.sync_state,
            };

            let data_val = serde_json::to_value(&item_data)?;

            remote_grocery_item_store_info_changes.push(GroceryItemStoreInfoChangeDelta {
                grocery_item_id: row.grocery_item_id,
                store_id: row.store_id,
                operation_type: if row.is_deleted { OperationType::Delete } else { OperationType::Update },
                version: row.version,
                data: Some(data_val),
            });
        }
    }

    Ok((
        remote_grocery_list_changes,
        remote_grocery_list_member_changes,
        remote_store_changes,
        remote_category_changes,
        remote_grocery_changes,
        remote_grocery_item_store_info_changes,
    ))
}
