use crate::routes::sync::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

pub async fn fetch_remote_grocery_mutations(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
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
        // Fetch grocery_lists changed after last_synced_at by OTHER clients where current user is a member or owner
        let updated_lists = sqlx::query!(
            r#"SELECT DISTINCT gl.id, gl.name, gl."ownerId" as owner_id, gl."createdAt" as created_at, gl.version, gl.is_deleted, gl.sync_state
               FROM grocery_lists gl
               LEFT JOIN grocery_list_members glm ON gl.id = glm."listId" AND glm.is_deleted = FALSE
               WHERE (gl."ownerId" = $1 OR glm."userId" = $1)
                 AND gl.updated_at > $2
                 AND (gl.updated_by_client != $3 OR gl.updated_by_client IS NULL)"#,
            user_id,
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

        // Fetch grocery_list_members changed after last_synced_at by OTHER clients for lists current user is a member of
        let updated_members = sqlx::query!(
            r#"SELECT DISTINCT glm.id, glm."listId" as list_id, glm."userId" as user_id, glm.role, glm."joinedAt" as joined_at, glm.version, glm.is_deleted, glm.sync_state
               FROM grocery_list_members glm
               JOIN grocery_list_members my_glm ON glm."listId" = my_glm."listId" AND my_glm.is_deleted = FALSE
               WHERE my_glm."userId" = $1
                 AND glm.updated_at > $2
                 AND (glm.updated_by_client != $3 OR glm.updated_by_client IS NULL)"#,
            user_id,
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

        // Fetch stores changed after last_synced_at by OTHER clients belonging to current user or lists user is member of
        let updated_stores = sqlx::query!(
            r#"SELECT DISTINCT s.id, s.name, s.position, s."isDefaultSupported" as is_default_supported, s."userId" as user_id, s.version, s.is_deleted, s.sync_state, s."listId" as list_id
               FROM stores s
               LEFT JOIN grocery_list_members glm ON s."listId" = glm."listId" AND glm."userId" = $1 AND glm.is_deleted = FALSE
               WHERE (s."userId" = $1 OR glm.id IS NOT NULL)
                 AND s.updated_at > $2
                 AND (s.updated_by_client != $3 OR s.updated_by_client IS NULL)"#,
            user_id,
            last_synced_at,
            client_id
        )
        .fetch_all(&mut **tx)
        .await?;

        for row in updated_stores {
            let item_id = row.id.clone();
            let item_data = StoreData {
                id: row.id,
                name: row.name,
                position: row.position,
                is_default_supported: row.is_default_supported,
                user_id: row.user_id,
                version: row.version,
                is_deleted: row.is_deleted,
                sync_state: row.sync_state,
                list_id: row.list_id,
            };
            let data_val = serde_json::to_value(&item_data)?;
            remote_store_changes.push(StoreChangeDelta {
                id: item_id,
                operation_type: if row.is_deleted { OperationType::Delete } else { OperationType::Update },
                version: row.version,
                data: Some(data_val),
            });
        }

        // Fetch categories changed after last_synced_at by OTHER clients belonging to current user or lists user is member of
        let updated_categories = sqlx::query!(
            r#"SELECT DISTINCT c.id, c.name, c.position, c."userId" as user_id, c.icon, c.version, c.is_deleted, c.sync_state, c."listId" as list_id
               FROM categories c
               LEFT JOIN grocery_list_members glm ON c."listId" = glm."listId" AND glm."userId" = $1 AND glm.is_deleted = FALSE
               WHERE (c."userId" = $1 OR glm.id IS NOT NULL)
                 AND c.updated_at > $2
                 AND (c.updated_by_client != $3 OR c.updated_by_client IS NULL)"#,
            user_id,
            last_synced_at,
            client_id
        )
        .fetch_all(&mut **tx)
        .await?;

        for row in updated_categories {
            let item_id = row.id.clone();
            let item_data = CategoryData {
                id: row.id,
                name: row.name,
                position: row.position,
                user_id: row.user_id,
                icon: row.icon,
                version: row.version,
                is_deleted: row.is_deleted,
                sync_state: row.sync_state,
                list_id: row.list_id,
            };
            let data_val = serde_json::to_value(&item_data)?;
            remote_category_changes.push(CategoryChangeDelta {
                id: item_id,
                operation_type: if row.is_deleted { OperationType::Delete } else { OperationType::Update },
                version: row.version,
                data: Some(data_val),
            });
        }

        // Fetch grocery_items changed after last_synced_at by OTHER clients belonging to lists current user is member of, or owned by user if listId is null
        let updated_groceries = sqlx::query!(
            r#"SELECT DISTINCT
                gi.id, gi.name, gi.quantity, gi."isBought" as is_bought, gi."createdAt" as created_at, gi.position, gi."categoryId" as category_id,
                gi."timesBought" as times_bought, gi."userId" as user_id, gi."isActive" as is_active, gi."listId" as list_id, gi.unit, gi.notes, gi.version, gi.is_deleted, gi.sync_state
               FROM grocery_items gi
               LEFT JOIN grocery_list_members glm ON gi."listId" = glm."listId" AND glm."userId" = $1 AND glm.is_deleted = FALSE
               WHERE (glm.id IS NOT NULL OR (gi."listId" IS NULL AND gi."userId" = $1))
                 AND gi.updated_at > $2
                 AND (gi.updated_by_client != $3 OR gi.updated_by_client IS NULL)"#,
            user_id,
            last_synced_at,
            client_id
        )
        .fetch_all(&mut **tx)
        .await?;

        for row in updated_groceries {
            let item_id = row.id.clone();
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
                id: item_id,
                operation_type: if row.is_deleted {
                    OperationType::Delete
                } else {
                    OperationType::Update
                },
                version: row.version,
                data: Some(data_val),
            });
        }

        // Fetch grocery_item_store_info changed after last_synced_at by OTHER clients belonging to current user or collaborative lists
        let updated_store_infos = sqlx::query!(
            r#"SELECT DISTINCT gsi."groceryItemId" as grocery_item_id, gsi."storeId" as store_id, gsi.price, gsi."isAvailable" as is_available, gsi."userId" as user_id, gsi.version, gsi.is_deleted, gsi.sync_state
               FROM grocery_item_store_info gsi
               JOIN grocery_items gi ON gsi."groceryItemId" = gi.id
               LEFT JOIN grocery_list_members glm ON gi."listId" = glm."listId" AND glm."userId" = $1 AND glm.is_deleted = FALSE
               WHERE (gsi."userId" = $1 OR gi."userId" = $1 OR glm.id IS NOT NULL)
                 AND gsi.updated_at > $2
                 AND (gsi.updated_by_client != $3 OR gsi.updated_by_client IS NULL)"#,
            user_id,
            last_synced_at,
            client_id
        )
        .fetch_all(&mut **tx)
        .await?;

        for row in updated_store_infos {
            let item_id = row.grocery_item_id.clone();
            let store_id = row.store_id.clone();
            let item_data = GroceryItemStoreInfoData {
                id: format!("{}-{}", row.grocery_item_id, row.store_id),
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
                id: format!("{}-{}", item_id, store_id),
                grocery_item_id: item_id,
                store_id: store_id,
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
