pub mod cdp;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tracing::{info, warn};

use crate::browser::cdp::{CdpError, CdpSession, StorageCookie, StorageState};
use crate::config::Config;
use crate::error::AppError;

// Keep X selectors in one place so UI changes require a single edit.
const COMPOSE_URL: &str = "https://x.com/compose/post";
const TEXTAREA: &str = r#"[data-testid="tweetTextarea_0"]"#;
const FILE_INPUT: &str = r#"input[data-testid="fileInput"]"#;
const ATTACHMENTS: &str = r#"[data-testid="attachments"]"#;
const POST_BTN: &str = r#"[data-testid="tweetButton"], [data-testid="tweetButtonInline"]"#;
const TOAST: &str = r#"[data-testid="toast"]"#;
const TOAST_STATUS_LINK: &str = r#"[data-testid="toast"] a[href*="/status/"]"#;

#[derive(Debug, Serialize)]
pub struct PostResult {
    pub ok: Option<bool>,
    pub status: &'static str,
    pub url: Option<String>,
}

pub fn has_session(config: &Config) -> bool {
    config.state_file.exists()
}

pub fn cookies_from_tokens(auth_token: &str, ct0: Option<&str>) -> Vec<StorageCookie> {
    let mut cookies = vec![StorageCookie {
        name: "auth_token".into(),
        value: auth_token.trim().into(),
        domain: ".x.com".into(),
        path: "/".into(),
        expires: -1.0,
        http_only: true,
        secure: true,
        same_site: Some("None".into()),
    }];
    if let Some(ct0_val) = ct0.map(str::trim).filter(|s| !s.is_empty()) {
        cookies.push(StorageCookie {
            name: "ct0".into(),
            value: ct0_val.into(),
            domain: ".x.com".into(),
            path: "/".into(),
            expires: -1.0,
            http_only: false,
            secure: true,
            same_site: Some("Lax".into()),
        });
    }
    cookies
}

pub fn cookies_from_header(raw_cookie: &str) -> Vec<StorageCookie> {
    raw_cookie
        .split(';')
        .filter_map(|part| {
            let (k, v) = part.trim().split_once('=')?;
            let name = k.trim();
            let value = v.trim().trim_matches('"');
            if name.is_empty() || value.is_empty() {
                return None;
            }
            Some(StorageCookie {
                name: name.into(),
                value: value.into(),
                domain: ".x.com".into(),
                path: "/".into(),
                expires: -1.0,
                http_only: name == "auth_token",
                secure: true,
                same_site: Some(if name == "auth_token" { "None" } else { "Lax" }.into()),
            })
        })
        .collect()
}

pub async fn run_browser(
    config: &Config,
    text: &str,
    media: &[PathBuf],
) -> Result<PostResult, AppError> {
    let state = load_or_init_state(config).await?;
    let mut page = CdpSession::launch(
        config.chrome_bin.as_deref(),
        config.headless,
        config.chrome_no_sandbox,
    )
    .await
    .map_err(|e| AppError::bad_gateway(e.to_string()))?;

    let outcome = execute_post(&mut page, config, state, text, media).await;
    if let Err(CdpError::Timeout) = &outcome {
        snap(&mut page, &config.debug_dir, "timeout").await;
    }
    outcome.map_err(map_cdp_err)
}

async fn execute_post(
    page: &mut CdpSession,
    config: &Config,
    mut state: StorageState,
    text: &str,
    media: &[PathBuf],
) -> Result<PostResult, CdpError> {
    page.set_cookies(&state.cookies).await?;
    info!("opening {COMPOSE_URL}");
    page.goto(COMPOSE_URL).await?;

    if page
        .wait_for_selector(TEXTAREA, Duration::from_secs(25))
        .await
        .is_err()
    {
        snap(page, &config.debug_dir, "compose-not-found").await;
        let url = page.current_url().await.unwrap_or_default();
        if url.contains("login") || url.contains("logout") {
            return Err(CdpError::Protocol("SESSION_EXPIRED".into()));
        }
        return Err(CdpError::Protocol("COMPOSE_NOT_FOUND".into()));
    }

    if !text.trim().is_empty() {
        info!("inserting caption ({} chars)", text.chars().count());
        page.click(TEXTAREA).await?;
        page.insert_text(text).await?;
    }

    if !media.is_empty() {
        info!("attaching {} media file(s)", media.len());
        page.set_input_files(FILE_INPUT, media).await?;
        page.wait_for_selector(ATTACHMENTS, Duration::from_secs(60))
            .await?;
    }

    // Post button stays disabled until X finishes processing text and uploaded media.
    let btn_sel = serde_json::to_string(POST_BTN).unwrap_or_default();
    let btn_ready_js = format!(
        "(() => {{ const b = document.querySelector({btn_sel}); return Boolean(b && b.getAttribute('aria-disabled') !== 'true' && !b.disabled); }})()"
    );
    info!("waiting for post button to become enabled");
    page.wait_for_js(&btn_ready_js, config.upload_timeout)
        .await?;
    info!("clicking post button");
    page.click(POST_BTN).await?;

    let post_url = match wait_post_completion(page, Duration::from_secs(30)).await {
        PostOutcome::Posted(url) => url,
        PostOutcome::Rejected(reason) => {
            snap(page, &config.debug_dir, "post-rejected").await;
            return Err(CdpError::Protocol(format!("POST_REJECTED:{reason}")));
        }
        PostOutcome::SessionExpired => {
            snap(page, &config.debug_dir, "session-expired").await;
            return Err(CdpError::Protocol("SESSION_EXPIRED".into()));
        }
        PostOutcome::Unconfirmed => {
            // Return 200 unconfirmed instead of an error so n8n does not retry and duplicate the post.
            snap(page, &config.debug_dir, "unconfirmed").await;
            warn!("post not confirmed");
            return Ok(PostResult {
                ok: None,
                status: "unconfirmed",
                url: None,
            });
        }
    };

    if let Ok(cookies) = page.get_cookies().await {
        state.cookies = cookies;
        let _ = save_storage_state(&config.state_file, &state).await;
    }

    info!("post confirmed: {:?}", post_url);
    Ok(PostResult {
        ok: Some(true),
        status: "posted",
        url: post_url,
    })
}

enum PostOutcome {
    Posted(Option<String>),
    Rejected(String),
    SessionExpired,
    Unconfirmed,
}

async fn wait_post_completion(page: &mut CdpSession, timeout: Duration) -> PostOutcome {
    let check_js = format!(
        r#"(() => {{
            const path = window.location.pathname;
            const href = document.querySelector({toast_link})?.getAttribute('href') ?? null;
            const toastEl = document.querySelector({toast});
            const toastText = toastEl ? (toastEl.innerText || '').trim() : null;
            const modalOpen = Boolean(document.querySelector('[data-testid="tweetButton"]')) || path.startsWith('/compose');
            return {{ path, href, toastText, modalOpen }};
        }})()"#,
        toast_link = serde_json::to_string(TOAST_STATUS_LINK).unwrap_or_default(),
        toast = serde_json::to_string(TOAST).unwrap_or_default(),
    );

    let start = std::time::Instant::now();
    loop {
        if let Ok(val) = page.eval(&check_js).await {
            let path = val.get("path").and_then(|v| v.as_str()).unwrap_or("");
            if path.contains("login") || path.contains("logout") || path.starts_with("/account") {
                return PostOutcome::SessionExpired;
            }

            if let Some(href) = val.get("href").and_then(|v| v.as_str()) {
                return PostOutcome::Posted(Some(normalize_x_url(href)));
            }

            let modal_open = val
                .get("modalOpen")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let toast_text = val
                .get("toastText")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();

            if modal_open && !toast_text.is_empty() {
                // Wait briefly in case the status link inside the toast renders a moment later.
                if let Ok(Some(late_href)) = page
                    .get_attribute(TOAST_STATUS_LINK, "href", Duration::from_millis(800))
                    .await
                {
                    return PostOutcome::Posted(Some(normalize_x_url(&late_href)));
                }
                return PostOutcome::Rejected(toast_text.to_string());
            }

            if !modal_open && path == "/home" {
                let late_href = page
                    .get_attribute(TOAST_STATUS_LINK, "href", Duration::from_secs(4))
                    .await
                    .ok()
                    .flatten()
                    .map(|h| normalize_x_url(&h));
                return PostOutcome::Posted(late_href);
            }
        }
        if start.elapsed() >= timeout {
            return PostOutcome::Unconfirmed;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn normalize_x_url(href: &str) -> String {
    if href.starts_with("http") {
        href.to_string()
    } else {
        format!("https://x.com{href}")
    }
}

fn map_cdp_err(err: CdpError) -> AppError {
    match err {
        CdpError::Timeout => AppError::gateway_timeout("timed out while posting"),
        CdpError::Protocol(msg) if msg == "SESSION_EXPIRED" => AppError::unauthorized(
            "session expired. Send a fresh session to POST /session.",
        ),
        CdpError::Protocol(msg) if msg == "COMPOSE_NOT_FOUND" => {
            AppError::bad_gateway("compose box not found (UI changed or blocked)")
        }
        CdpError::Protocol(msg) if msg.starts_with("POST_REJECTED:") => {
            let reason = msg.trim_start_matches("POST_REJECTED:");
            AppError::bad_gateway(format!("post rejected by X: {reason}"))
        }
        other => AppError::bad_gateway(other.to_string()),
    }
}

async fn snap(page: &mut CdpSession, debug_dir: &Path, name: &str) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = debug_dir.join(format!("{ts}-{name}.png"));
    let _ = page.screenshot(&path).await;
}

async fn load_or_init_state(config: &Config) -> Result<StorageState, AppError> {
    if config.state_file.exists() {
        let raw = tokio::fs::read_to_string(&config.state_file)
            .await
            .map_err(|e| AppError::service_unavailable(format!("cannot read session file: {e}")))?;
        let state: StorageState = serde_json::from_str(&raw)
            .map_err(|e| AppError::service_unavailable(format!("invalid session file: {e}")))?;
        if !state.cookies.is_empty() {
            return Ok(state);
        }
    }

    Err(AppError::service_unavailable(
        "no session configured. Call POST /session first.",
    ))
}

pub async fn save_storage_state(path: &Path, state: &StorageState) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| AppError::bad_gateway(format!("failed to create data dir: {e}")))?;
    }
    let body = serde_json::to_string_pretty(state)
        .map_err(|e| AppError::bad_gateway(format!("failed to serialize session: {e}")))?;
    tokio::fs::write(path, body)
        .await
        .map_err(|e| AppError::bad_gateway(format!("failed to write session file: {e}")))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}
