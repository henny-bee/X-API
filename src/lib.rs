pub mod browser;
pub mod config;
pub mod error;
pub mod media;
pub mod routes;
pub mod state;

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::config::Config;
    use crate::routes::router;
    use crate::state::AppState;

    fn test_state() -> AppState {
        AppState::new(Config {
            api_token: "secret-token".into(),
            host: "127.0.0.1".into(),
            port: 8000,
            debug_dir: PathBuf::from("./data/debug"),
            headless: true,
            min_interval: Duration::from_secs(60),
            upload_timeout: Duration::from_secs(300),
            max_media_bytes: 10 * 1024 * 1024,
            max_text_chars: 280,
            allow_private_media_urls: false,
            chrome_bin: None,
            chrome_no_sandbox: false,
        })
    }

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let app = router(test_state());
        let res = app
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["ok"], true);
    }

    #[tokio::test]
    async fn post_rejects_missing_or_wrong_token() {
        let app = router(test_state());
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/post")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"text":"hello"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn post_requires_auth_token_cookie_and_validates_payload() {
        let app = router(test_state());
        let missing_cookie = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/post")
                    .header("content-type", "application/json")
                    .header("x-token", "secret-token")
                    .body(Body::from(r#"{"text":"hello"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_cookie.status(), StatusCode::BAD_REQUEST);

        let empty_text = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/post")
                    .header("content-type", "application/json")
                    .header("x-token", "secret-token")
                    .header("x-auth-token", "cookie_123")
                    .body(Body::from(r#"{"text":"   "}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(empty_text.status(), StatusCode::BAD_REQUEST);
    }
}
