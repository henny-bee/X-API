pub mod health;
pub mod post;
pub mod session;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use subtle::ConstantTimeEq;

use crate::error::AppError;
use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health::health))
        .route(
            "/session",
            post(session::set_session).delete(session::delete_session),
        )
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
