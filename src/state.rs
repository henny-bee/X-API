use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::Semaphore;

use crate::browser::cdp::StorageCookie;
use crate::browser::{self, PostResult};
use crate::config::Config;
use crate::error::AppError;

#[derive(Default)]
struct AccountTracker {
    in_flight: HashSet<String>,
    last_post: HashMap<String, Instant>,
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    tracker: Arc<Mutex<AccountTracker>>,
    browser_permits: Arc<Semaphore>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(config),
            tracker: Arc::new(Mutex::new(AccountTracker::default())),
            // Cap concurrent Chromium instances so multi-user requests do not exhaust container RAM.
            browser_permits: Arc::new(Semaphore::new(2)),
        }
    }

    pub async fn publish(
        &self,
        account_key: &str,
        cookies: &[StorageCookie],
        text: &str,
        media: &[PathBuf],
    ) -> Result<PostResult, AppError> {
        {
            let mut tracker = self.tracker.lock().unwrap();
            if tracker.in_flight.contains(account_key) {
                return Err(AppError::conflict(
                    "another post is already in progress for this X account",
                ));
            }
            if let Some(prev) = tracker.last_post.get(account_key) {
                let elapsed = prev.elapsed();
                if elapsed < self.config.min_interval {
                    let wait = (self.config.min_interval - elapsed).as_secs() + 1;
                    return Err(AppError::too_many_requests(wait));
                }
            }
            tracker.in_flight.insert(account_key.to_string());
        }

        struct InFlightGuard<'a> {
            tracker: &'a Mutex<AccountTracker>,
            key: String,
        }
        impl Drop for InFlightGuard<'_> {
            fn drop(&mut self) {
                if let Ok(mut t) = self.tracker.lock() {
                    t.in_flight.remove(&self.key);
                }
            }
        }

        let _guard = InFlightGuard {
            tracker: &self.tracker,
            key: account_key.to_string(),
        };

        let _permit = self
            .browser_permits
            .acquire()
            .await
            .map_err(|_| AppError::service_unavailable("server shutting down"))?;

        let result = browser::run_browser(&self.config, cookies, text, media).await?;
        if result.ok != Some(false) {
            if let Ok(mut tracker) = self.tracker.lock() {
                tracker.last_post.insert(account_key.to_string(), Instant::now());
            }
        }
        Ok(result)
    }
}
