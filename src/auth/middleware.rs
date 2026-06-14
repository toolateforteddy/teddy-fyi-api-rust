use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::Response,
};
use jsonwebtoken::{decode, DecodingKey, Validation, Algorithm};
use crate::auth::tokens::Claims;
use crate::state::AppState;

pub async fn require_auth(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let auth_header = req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));

    let token = if let Some(token_val) = auth_header {
        token_val.to_string()
    } else {
        req.headers()
            .get(header::COOKIE)
            .and_then(|h| h.to_str().ok())
            .and_then(|cookie_str| {
                cookie_str.split(';')
                    .map(|s| s.trim())
                    .find(|s| s.starts_with("access_token="))
                    .and_then(|s| s.strip_prefix("access_token="))
            })
            .ok_or(StatusCode::UNAUTHORIZED)?
            .to_string()
    };

    let client_uuid_header = req.headers()
        .get("X-Client-UUID")
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;

    let secret = &state.jwt_secret;

    let token_data = decode::<Claims>(
        &token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::new(Algorithm::HS256),
    ).map_err(|_| StatusCode::UNAUTHORIZED)?;

    if token_data.claims.client_uuid != client_uuid_header {
        return Err(StatusCode::FORBIDDEN);
    }

    let mut req = req;
    req.extensions_mut().insert(token_data.claims);
    Ok(next.run(req).await)
}
