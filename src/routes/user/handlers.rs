use axum::{extract::State, Extension, Json};
use serde::{Deserialize, Serialize};
use crate::auth::tokens::Claims;
use crate::routes::user::deletion::{announce_deletion, delete_user_data, DeletedCounts};
use crate::routes::sync::types::AppError;
use crate::state::AppState;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DeleteUserDataResponse {
    pub user_id: String,
    pub deleted: DeletedCounts,
}

/// `DELETE /api/user/data` — erase everything the authenticated account owns.
///
/// Scoped entirely to the caller's own claims: there is no way to name another user. The
/// erase itself lives in [`delete_user_data`], which the scheduled reaper shares.
pub async fn delete_user_data_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<DeleteUserDataResponse>, AppError> {
    let user_id = claims.sub.clone();

    let mut tx = state.db_pool.begin().await?;
    let (deleted, affected_users) = delete_user_data(&mut tx, &user_id).await?;
    tx.commit().await?;

    tracing::info!(user_id = %user_id, deleted = ?deleted, "Deleted all data for user");

    announce_deletion(&state.redis_publisher, &user_id, &affected_users).await;

    Ok(Json(DeleteUserDataResponse { user_id, deleted }))
}
