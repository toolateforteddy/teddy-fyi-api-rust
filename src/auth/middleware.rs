use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use jsonwebtoken::{decode, DecodingKey, Validation, Algorithm};
use crate::auth::tokens::Claims;
use crate::state::AppState;

// The `Err` variant is an axum `Response`, which is what `from_fn_with_state` requires
// here — boxing it would drop the `IntoResponse` impl that `Result<T, E>` needs on both
// variants, so this lint has no actionable fix at this call site.
#[allow(clippy::result_large_err)]
pub async fn require_auth(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, Response> {
    let client_uuid_header = match req.headers()
        .get("X-Client-UUID")
        .and_then(|h| h.to_str().ok())
    {
        Some(uuid) => uuid.to_string(),
        None => {
            tracing::warn!("Authentication failed: X-Client-UUID header is missing");
            return Err((
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({ "error": "Missing X-Client-UUID header" })),
            ).into_response());
        }
    };

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
                tracing::warn!(
                    client_uuid = %client_uuid_header,
                    "Authentication failed: access token cookie or Bearer token is missing"
                );
                return Err((
                    StatusCode::UNAUTHORIZED,
                    axum::Json(serde_json::json!({ "error": "Missing access token" })),
                ).into_response());
            }
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
            tracing::warn!(
                client_uuid = %client_uuid_header,
                "Authentication failed: invalid token: {:?}",
                err
            );
            return Err((
                StatusCode::UNAUTHORIZED,
                axum::Json(serde_json::json!({ "error": format!("Invalid token: {}", err) })),
            ).into_response());
        }
    };

    if token_data.claims.client_uuid != client_uuid_header {
        tracing::warn!(
            client_uuid = %client_uuid_header,
            token_client_uuid = %token_data.claims.client_uuid,
            "Authentication failed: client UUID mismatch"
        );
        return Err((
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({ "error": "Client UUID mismatch" })),
        ).into_response());
    }

    let mut req = req;
    // Cloned before the claims move into the extensions; the request log needs it
    // on the way back out, by which point `req` is gone.
    let user_id = token_data.claims.sub.clone();
    req.extensions_mut().insert(token_data.claims);

    let mut response = next.run(req).await;
    // Hashed, never raw — see `observability::http::LoggedUser` for why.
    let salt = crate::observability::http::log_hash_salt(&state.jwt_secret);
    response
        .extensions_mut()
        .insert(crate::observability::http::LoggedUser(
            crate::observability::http::hash_user_id(&user_id, &salt),
        ));
    Ok(response)
}
