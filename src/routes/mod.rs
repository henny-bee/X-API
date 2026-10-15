pub mod health;
pub mod post;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use subtle::ConstantTimeEq;

use crate::browser::cdp::StorageCookie;
use crate::browser::cookies_from_tokens;
use crate::error::AppError;
use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health::health))
        .route("/post", post(post::post_json))
        .route(
            "/post/upload",
            post(post::post_upload).layer(DefaultBodyLimit::disable()),
        )
        .with_state(state)
}

pub fn require_auth(headers: &HeaderMap, expected: &str) -> Result<(), AppError> {
    let provided = headers
        .get("x-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let is_match = provided.len() == expected.len()
        && bool::from(provided.as_bytes().ct_eq(expected.as_bytes()));

    if !is_match {
        return Err(AppError::unauthorized("unauthorized"));
    }
    Ok(())
}

pub fn extract_session_cookies(
    headers: &HeaderMap,
    body_auth_token: Option<&str>,
    body_ct0: Option<&str>,
) -> Result<(String, Vec<StorageCookie>), AppError> {
    let auth_token = body_auth_token
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            headers
                .get("x-auth-token")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .ok_or_else(|| {
            AppError::bad_request("missing X session: provide x-auth-token header or auth_token field")
        })?;

    let ct0 = body_ct0
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            headers
                .get("x-ct0")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        });

    Ok((auth_token.to_string(), cookies_from_tokens(auth_token, ct0)))
}
