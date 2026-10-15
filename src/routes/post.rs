use std::path::PathBuf;

use axum::Json;
use axum::extract::{Multipart, State};
use axum::http::HeaderMap;
use serde::Deserialize;
use tempfile::TempDir;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;

use crate::browser::PostResult;
use crate::error::AppError;
use crate::media;
use crate::routes::require_auth;
use crate::state::AppState;

#[derive(Default, Deserialize)]
pub struct PostIn {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub media_urls: Vec<String>,
}

pub async fn post_json(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PostIn>,
) -> Result<Json<PostResult>, AppError> {
    require_auth(&headers, &state.config.api_token)?;

    let tmp = TempDir::new()
        .map_err(|e| AppError::bad_gateway(format!("failed to create temp dir: {e}")))?;

    let mut files = Vec::with_capacity(body.media_urls.len());
    for url in &body.media_urls {
        let path = media::download_media(url, tmp.path(), &state.config).await?;
        files.push(path);
    }

    media::validate_post(&body.text, &files, state.config.max_text_chars)?;
    let result = state.publish(&body.text, &files).await?;
    Ok(Json(result))
}

pub async fn post_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<PostResult>, AppError> {
    require_auth(&headers, &state.config.api_token)?;

    let tmp = TempDir::new()
        .map_err(|e| AppError::bad_gateway(format!("failed to create temp dir: {e}")))?;
    let max_mb = state.config.max_media_bytes / (1024 * 1024);

    let mut text = String::new();
    let mut files: Vec<PathBuf> = Vec::new();

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("invalid multipart payload: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name == "text" && field.file_name().is_none() {
            text = field
                .text()
                .await
                .map_err(|e| AppError::bad_request(format!("invalid text field: {e}")))?;
            continue;
        }

        if name == "files" || field.file_name().is_some() {
            let filename = field.file_name().unwrap_or("").to_string();
            let content_type = field.content_type().unwrap_or("").to_string();
            let ext = media::ext_for(&filename, &content_type);
            if !media::is_allowed_ext(&ext) {
                return Err(AppError::unsupported_media(format!(
                    "unsupported file type: {filename}"
                )));
            }

            let dest = tmp.path().join(media::random_filename(&ext));
            let mut out = File::create(&dest)
                .await
                .map_err(|e| AppError::bad_gateway(format!("failed to create temp file: {e}")))?;

            let mut size: u64 = 0;
            while let Some(chunk) = field
                .chunk()
                .await
                .map_err(|e| AppError::bad_request(format!("error reading upload: {e}")))?
            {
                size = size.saturating_add(chunk.len() as u64);
                if size > state.config.max_media_bytes {
                    return Err(AppError::payload_too_large(format!(
                        "file larger than {max_mb} MB"
                    )));
                }
                out.write_all(&chunk)
                    .await
                    .map_err(|e| AppError::bad_gateway(format!("failed to write upload: {e}")))?;
            }
            out.flush()
                .await
                .map_err(|e| AppError::bad_gateway(format!("failed to flush upload: {e}")))?;
            files.push(dest);
        }
    }

    media::validate_post(&text, &files, state.config.max_text_chars)?;
    let result = state.publish(&text, &files).await?;
    Ok(Json(result))
}
