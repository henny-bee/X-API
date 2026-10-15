use std::env;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    pub api_token: String,
    pub host: String,
    pub port: u16,
    pub state_file: PathBuf,
    pub debug_dir: PathBuf,
    pub headless: bool,
    pub min_interval: Duration,
    pub upload_timeout: Duration,
    pub max_media_bytes: u64,
    pub max_text_chars: usize,
    pub allow_private_media_urls: bool,
    pub chrome_bin: Option<String>,
    pub chrome_no_sandbox: bool,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let api_token = env::var("API_TOKEN").unwrap_or_default();
        if api_token.is_empty() {
            return Err("Set API_TOKEN env var (long random string).".into());
        }

        let max_media_mb: u64 = env_parse("MAX_MEDIA_MB", 512)?;
        Ok(Self {
            api_token,
            host: env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into()),
            port: env_parse("PORT", 8000)?,
            state_file: PathBuf::from(
                env::var("STATE_FILE").unwrap_or_else(|_| "./data/storage_state.json".into()),
            ),
            debug_dir: PathBuf::from(
                env::var("DEBUG_DIR").unwrap_or_else(|_| "./data/debug".into()),
            ),
            headless: !env::var("HEADLESS")
                .map(|v| v.eq_ignore_ascii_case("false"))
                .unwrap_or(false),
            min_interval: Duration::from_secs(env_parse("MIN_INTERVAL_SECONDS", 60)?),
            upload_timeout: Duration::from_secs(env_parse("UPLOAD_TIMEOUT_SECONDS", 300)?),
            max_media_bytes: max_media_mb.saturating_mul(1024 * 1024),
            max_text_chars: env_parse("MAX_TEXT_CHARS", 280)?,
            allow_private_media_urls: env::var("ALLOW_PRIVATE_MEDIA_URLS")
                .map(|v| v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            chrome_bin: non_empty_env("CHROME_BIN"),
            chrome_no_sandbox: env::var("CHROME_NO_SANDBOX")
                .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
                .unwrap_or(false),
        })
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> Result<T, String> {
    match env::var(key) {
        Ok(val) if !val.is_empty() => val
            .parse::<T>()
            .map_err(|_| format!("invalid value for {key}: {val}")),
        _ => Ok(default),
    }
}
