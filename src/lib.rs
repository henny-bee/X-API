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
    use tempfile::TempDir;
    use tower::ServiceExt;

    use crate::config::Config;
    use crate::routes::router;
    use crate::state::AppState;

    fn test_state(state_file: PathBuf) -> AppState {
        AppState::new(Config {
            api_token: "secret-token".into(),
            host: "127.0.0.1".into(),
            port: 8000,
            state_file,
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
    async fn health_endpoint_reports_session_status() {
        let app = router(test_state(PathBuf::from("./nonexistent_session_for_test.json")));
        let res = app
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["ok"], true);
        assert_eq!(json["session_saved"], false);
    }

    #[tokio::test]
    async fn session_manual_input_and_delete_work() {
        let tmp = TempDir::new().unwrap();
        let state_file = tmp.path().join("storage_state.json");
        let app = router(test_state(state_file.clone()));

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/session")
                    .header("content-type", "application/json")
                    .header("x-token", "secret-token")
                    .body(Body::from(
                        r#"{"auth_token":"my_auth_token_123","ct0":"my_ct0_456"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(state_file.exists());

        let health_res = app
            .clone()
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = health_res.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["session_saved"], true);

        let del_res = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/session")
                    .header("x-token", "secret-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(del_res.status(), StatusCode::OK);
        assert!(!state_file.exists());
    }

    #[tokio::test]
    async fn post_rejects_missing_or_wrong_token() {
        let app = router(test_state(PathBuf::from("./nonexistent_session_for_test.json")));
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
    async fn post_validates_empty_payload_and_missing_session() {
        let app = router(test_state(PathBuf::from("./nonexistent_session_for_test.json")));
        let empty_res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/post")
                    .header("content-type", "application/json")
                    .header("x-token", "secret-token")
                    .body(Body::from(r#"{"text":"   "}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(empty_res.status(), StatusCode::BAD_REQUEST);

        let no_session_res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/post")
                    .header("content-type", "application/json")
                    .header("x-token", "secret-token")
                    .body(Body::from(r#"{"text":"hello world"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(no_session_res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
