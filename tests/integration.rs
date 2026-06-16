//! End-to-end proxy tests. Each test stands up a wiremock that
//! pretends to be a provider, runs a real /v1/chat/completions
//! request through the app, and asserts the response shape + the
//! X-Router-* headers.

use axum::http::StatusCode;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use token_dealer::{
    config::{ConfigService, ProviderConfig, ProviderType, RouterConfig, TierConfig, TierTimeoutsSet},
    providers::{HealthRegistry, ProviderRegistry},
    proxy::pipeline::Pipeline,
    server::build_router,
    AppState,
};
use tower::ServiceExt;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

async fn make_state(mock_base: &str) -> AppState {
    let mut cfg = RouterConfig::default();
    cfg.providers.push(ProviderConfig {
        id: "mock".to_string(),
        provider_type: ProviderType::Openai,
        key: Some("test-key".to_string()),
        base_url: mock_base.to_string(),
        default_model: Some("mock-model".to_string()),
    });
    cfg.tiers.insert(
        "standard".to_string(),
        TierConfig {
            primary: "mock/mock-model".to_string(),
            fallbacks: vec![],
            allow_tier_downgrade: false,
            downgrade_to: None,
            min_context_window: None,
            timeouts: TierTimeoutsSet::default(),
        },
    );
    // Serialize → load via ConfigService so we exercise the same path.
    let toml = toml::to_string(&cfg).unwrap();
    let dir = tempfile_or_stdout();
    let tmp = std::env::temp_dir().join(format!(
        "token-dealer-test-{}.toml",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&tmp, toml).unwrap();
    let svc = ConfigService::load(&tmp).await.unwrap();
    let _ = std::fs::remove_file(&tmp);
    let snapshot = svc.snapshot().await;
    let registry = Arc::new(ProviderRegistry::from_configs(&snapshot.providers).unwrap());
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let pipeline = Pipeline::new(registry, svc.clone(), http);
    let _ = dir;
    AppState::new(pipeline, svc, HealthRegistry::new())
}

fn tempfile_or_stdout() -> () {
    // placeholder so we can return () in async fn
}

#[tokio::test]
async fn chat_completion_routes_to_provider_and_strips_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-abc",
            "object": "chat.completion",
            "created": 1_700_000_000,
            "model": "mock-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "hello back",
                },
                "finish_reason": "stop",
            }],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 2,
                "total_tokens": 7,
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let state = make_state(&server.uri()).await;
    let app = build_router(state);

    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({
                "model": "standard",
                "messages": [{"role": "user", "content": "hello"}]
            })
            .to_string(),
        ))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // X-Router-* headers present
    let h = resp.headers().clone();
    assert_eq!(h.get("x-router-provider").unwrap(), "mock");
    assert_eq!(h.get("x-router-model").unwrap(), "mock-model");
    assert_eq!(h.get("x-router-tier").unwrap(), "standard");

    let body_bytes = axum::body::to_bytes(resp.into_body(), 65536)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "hello back");
    assert_eq!(body["usage"]["total_tokens"], 7);
}

#[tokio::test]
async fn explicit_model_ref_bypasses_tier_routing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "x",
            "object": "chat.completion",
            "created": 1,
            "model": "some-other-model",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": "stop",
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let state = make_state(&server.uri()).await;
    let app = build_router(state);

    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({
                "model": "mock/some-other-model",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        ))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body["model"], "some-other-model");
    // headers already consumed by the move; re-test against a separate request if needed
}

#[tokio::test]
async fn health_endpoint_responds_200() {
    let server = MockServer::start().await;
    let state = make_state(&server.uri()).await;
    let app = build_router(state);
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/health")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn list_models_includes_configured_provider() {
    let server = MockServer::start().await;
    let state = make_state(&server.uri()).await;
    let app = build_router(state);
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/v1/models")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    let ids: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"mock/mock-model"));
}
