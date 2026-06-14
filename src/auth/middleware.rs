use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use jsonwebtoken::{decode, DecodingKey, Validation, Algorithm};
use crate::auth::tokens::Claims;
use crate::state::AppState;

pub async fn require_auth(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, Response> {
    let auth_header = req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));

    let token = if let Some(token_val) = auth_header {
        token_val.to_string()
    } else {
        match req.headers()
            .get(header::COOKIE)
            .and_then(|h| h.to_str().ok())
            .and_then(|cookie_str| {
                cookie_str.split(';')
                    .map(|s| s.trim())
                    .find(|s| s.starts_with("access_token="))
                    .and_then(|s| s.strip_prefix("access_token="))
            })
        {
            Some(token) => token.to_string(),
            None => {
                tracing::warn!("Authentication failed: access token cookie or Bearer token is missing");
                return Err((
                    StatusCode::UNAUTHORIZED,
                    axum::Json(serde_json::json!({ "error": "Missing access token" })),
                ).into_response());
            }
        }
    };

    let client_uuid_header = match req.headers()
        .get("X-Client-UUID")
        .and_then(|h| h.to_str().ok())
    {
        Some(uuid) => uuid,
        None => {
            tracing::warn!("Authentication failed: X-Client-UUID header is missing");
            return Err((
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({ "error": "Missing X-Client-UUID header" })),
            ).into_response());
        }
    };

    let secret = &state.jwt_secret;

    let token_data = match decode::<Claims>(
        &token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::new(Algorithm::HS256),
    ) {
        Ok(data) => data,
        Err(err) => {
            tracing::warn!("Authentication failed: invalid token: {:?}", err);
            return Err((
                StatusCode::UNAUTHORIZED,
                axum::Json(serde_json::json!({ "error": format!("Invalid token: {}", err) })),
            ).into_response());
        }
    };

    if token_data.claims.client_uuid != client_uuid_header {
        tracing::warn!(
            "Authentication failed: client UUID mismatch. Token claims: {}, X-Client-UUID header: {}",
            token_data.claims.client_uuid,
            client_uuid_header
        );
        return Err((
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({ "error": "Client UUID mismatch" })),
        ).into_response());
    }

    let mut req = req;
    req.extensions_mut().insert(token_data.claims);
    Ok(next.run(req).await)
}
