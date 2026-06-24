use axum::{
    extract::State,
    Extension,
    Json,
};
use serde::{Deserialize, Serialize};
use crate::state::AppState;
use crate::auth::tokens::Claims;
use crate::routes::sync::types::AppError;
use rand::RngExt;
use rand::distr::Alphanumeric;
use chrono::Utc;

#[derive(Deserialize)]
pub struct InviteRequest {
    #[serde(alias = "list_id", rename = "listId")]
    pub list_id: String,
}

#[derive(Serialize)]
pub struct InviteResponse {
    pub code: String,
}

#[derive(Deserialize)]
pub struct JoinRequest {
    pub code: String,
}

#[derive(Serialize)]
pub struct JoinResponse {
    pub success: bool,
    #[serde(rename = "listId")]
    pub list_id: String,
}

pub async fn invite_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(payload): Json<InviteRequest>,
) -> Result<Json<InviteResponse>, AppError> {
    let user_id = &claims.sub;
    let list_id = &payload.list_id;

    // 1. Verify that the requesting user is a member of the grocery list
    let is_member = sqlx::query!(
        r#"SELECT 1 as dummy FROM grocery_list_members WHERE "listId" = $1 AND "userId" = $2 AND is_deleted = FALSE"#,
        list_id,
        user_id
    )
    .fetch_optional(&state.db_pool)
    .await?
    .is_some();

    if !is_member {
        return Err(AppError::Forbidden(format!(
            "User is not a member of grocery list {}",
            list_id
        )));
    }

    // 2. Generate a unique code (8 uppercase alphanumeric characters)
    let code: String = loop {
        let candidate: String = rand::rng()
            .sample_iter(Alphanumeric)
            .filter(|c| c.is_ascii_alphanumeric())
            .take(8)
            .map(|c| (c as char).to_ascii_uppercase())
            .collect();

        // Ensure uniqueness of the code
        let exists = sqlx::query!(
            "SELECT 1 as dummy FROM list_invites WHERE code = $1",
            candidate
        )
        .fetch_optional(&state.db_pool)
        .await?
        .is_some();

        if !exists {
            break candidate;
        }
    };

    // 3. Store in the database with a 24-hour expiry
    let expires_at = Utc::now() + chrono::Duration::hours(24);

    sqlx::query!(
        r#"INSERT INTO list_invites (code, "listId", "createdBy", "expiresAt")
           VALUES ($1, $2, $3, $4)"#,
        code,
        list_id,
        user_id,
        expires_at
    )
    .execute(&state.db_pool)
    .await?;

    Ok(Json(InviteResponse { code }))
}

pub async fn join_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(payload): Json<JoinRequest>,
) -> Result<Json<JoinResponse>, AppError> {
    let user_id = &claims.sub;
    let code = payload.code.trim().to_ascii_uppercase();

    let mut tx = state.db_pool.begin().await?;

    // 1. Validate the code exists and has not expired
    let invite = sqlx::query!(
        r#"SELECT "listId" as list_id, "expiresAt" as expires_at FROM list_invites WHERE code = $1"#,
        code
    )
    .fetch_optional(&mut *tx)
    .await?;

    let invite = match invite {
        Some(invite) => invite,
        None => {
            return Err(AppError::Forbidden("Invalid invite code".to_string()));
        }
    };

    if invite.expires_at < Utc::now() {
        // Clean up expired invite code
        let _ = sqlx::query!("DELETE FROM list_invites WHERE code = $1", code)
            .execute(&mut *tx)
            .await;
        tx.commit().await?;
        return Err(AppError::Forbidden("Expired invite code".to_string()));
    }

    let list_id = invite.list_id;

    // 2. Create or re-activate list membership for current_user
    let member_id = format!("{}-member-{}", list_id, user_id);
    let joined_at = Utc::now().timestamp_millis();

    sqlx::query!(
        r#"INSERT INTO grocery_list_members (
            id, "listId", "userId", role, "joinedAt", version, is_deleted, sync_state, updated_at, updated_by_client
        ) VALUES ($1, $2, $3, $4, $5, 1, FALSE, 'SYNCED', NOW(), NULL)
        ON CONFLICT (id) DO UPDATE SET
            is_deleted = FALSE,
            version = grocery_list_members.version + 1,
            updated_at = NOW(),
            updated_by_client = NULL"#,
        member_id,
        list_id,
        user_id,
        "MEMBER",
        joined_at
    )
    .execute(&mut *tx)
    .await?;

    // 3. Delete the invite code (single-use)
    sqlx::query!("DELETE FROM list_invites WHERE code = $1", code)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    Ok(Json(JoinResponse {
        success: true,
        list_id,
    }))
}
