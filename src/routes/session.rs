use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::browser::cdp::{StorageCookie, StorageState};
use crate::browser::{cookies_from_header, cookies_from_tokens, save_storage_state};
use crate::error::AppError;
use crate::routes::require_auth;
use crate::state::AppState;

#[derive(Default, Deserialize)]
pub struct SessionIn {
    #[serde(default)]
    pub auth_token: Option<String>,
    #[serde(default)]
    pub ct0: Option<String>,
    #[serde(default)]
    pub cookie: Option<String>,
    #[serde(default)]
    pub cookies: Vec<StorageCookie>,
}

pub async fn set_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SessionIn>,
) -> Result<Json<Value>, AppError> {
    require_auth(&headers, &state.config.api_token)?;

    let cookies = if !body.cookies.is_empty() {
        body.cookies
    } else if let Some(raw_cookie) = body.cookie.as_deref().filter(|s| !s.trim().is_empty()) {
        cookies_from_header(raw_cookie)
    } else if let Some(token) = body.auth_token.as_deref().filter(|s| !s.trim().is_empty()) {
        cookies_from_tokens(token, body.ct0.as_deref())
    } else {
        return Err(AppError::bad_request(
            "provide auth_token (and optional ct0), cookie header string, or cookies array",
        ));
    };

    let has_auth_token = cookies
        .iter()
        .any(|c| c.name == "auth_token" && !c.value.trim().is_empty());
    if !has_auth_token {
        return Err(AppError::bad_request("missing non-empty auth_token cookie"));
    }

    let storage = StorageState {
        cookies,
        origins: Vec::new(),
    };
    save_storage_state(&state.config.state_file, &storage).await?;

    Ok(Json(json!({
        "ok": true,
        "session_saved": true,
        "cookie_count": storage.cookies.len(),
    })))
}

pub async fn delete_session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    require_auth(&headers, &state.config.api_token)?;

    if state.config.state_file.exists() {
        tokio::fs::remove_file(&state.config.state_file)
            .await
            .map_err(|e| AppError::bad_gateway(format!("failed to remove session file: {e}")))?;
    }

    Ok(Json(json!({
        "ok": true,
        "session_saved": false,
    })))
}
