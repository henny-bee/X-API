use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};

use crate::browser;
use crate::state::AppState;

pub async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "session_saved": browser::has_session(&state.config),
    }))
}
