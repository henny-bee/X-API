use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

use crate::browser::{self, PostResult};
use crate::config::Config;
use crate::error::AppError;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub post_lock: Arc<Mutex< Option<Instant> >>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(config),
            post_lock: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn publish(&self, text: &str, media: &[PathBuf]) -> Result<PostResult, AppError> {
        let mut last_post = self
            .post_lock
            .try_lock()
            .map_err(|_| AppError::conflict("another post is in progress"))?;

        if let Some(prev) = *last_post {
            let elapsed = prev.elapsed();
            if elapsed < self.config.min_interval {
                let wait = (self.config.min_interval - elapsed).as_secs() + 1;
                return Err(AppError::too_many_requests(wait));
            }
        }

        let result = browser::run_browser(&self.config, text, media).await?;
        if result.ok != Some(false) {
            *last_post = Some(Instant::now());
        }
        Ok(result)
    }
}
