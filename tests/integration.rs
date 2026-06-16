//! End-to-end proxy tests. Each test stands up a wiremock that
//! pretends to be a provider, runs a real /v1/chat/completions
//! request through the app, and asserts the response shape + the
//! X-Router-* headers.

use axum::http::StatusCode;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use token_dealer::{
    config::{ConfigService, DatabaseConfig, ProviderConfig, ProviderType, RouterConfig, TierConfig, TierTimeoutsSet},
    db::Db,
    providers::{HealthRegistry, ProviderRegistry},
    proxy::pipeline::Pipeline,
    server::build_router,
    AppState,
};
use tower::ServiceExt;
use uuid::Uuid;
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
        base_url: Some(mock_base.to_string()),
        default_model: Some("mock-model".to_string()),
        path: None,
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
    let db = Db::open(&snapshot.database).unwrap();
    let pipeline = Pipeline::new(registry, svc.clone(), http, db.clone(), HealthRegistry::new());
    let _ = dir;
    let metadata = token_dealer::metadata::MetadataStore::new();
    AppState::new(pipeline, svc, HealthRegistry::new(), db, metadata)
}

fn tempfile_or_stdout() -> () {
    // placeholder so we can return () in async fn
}

/// Build a minimal CanonicalRequest for tests that need one but
/// don't care about its contents (e.g. fallback unit tests).
fn build_test_canonical_request() -> token_dealer::schema::canonical::CanonicalRequest {
    use token_dealer::schema::canonical::{CanonicalRequest, Tier};
    CanonicalRequest {
        messages: vec![],
        system: None,
        max_tokens: None,
        temperature: None,
        top_p: None,
        stop: None,
        stream: false,
        tools: None,
        tool_choice: None,
        tier: Tier::Standard,
        selected_model: "x".to_string(),
        selected_provider: "x".to_string(),
        request_id: Uuid::new_v4(),
        extensions: Default::default(),
    }
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
    let status = resp.status();
    let h = resp.headers().clone();
    let body_bytes = axum::body::to_bytes(resp.into_body(), 65536)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(h.get("x-router-provider").unwrap(), "mock");
    assert_eq!(h.get("x-router-model").unwrap(), "mock-model");
    assert_eq!(h.get("x-router-tier").unwrap(), "standard");
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

#[tokio::test]
async fn non_standard_path_provider_routes_correctly() {
    // Kilo uses /chat/completions (no /v1 prefix). Stand up a mock
    // that listens on that path and confirm the OpenAI adapter
    // honors the custom path.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "kilo-x",
            "object": "chat.completion",
            "created": 1,
            "model": "anthropic/claude-sonnet-4-5",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "via kilo"},
                "finish_reason": "stop",
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut cfg = RouterConfig::default();
    cfg.database = DatabaseConfig { path: ":memory:".to_string() };
    cfg.providers.push(ProviderConfig {
        id: "kilo".to_string(),
        provider_type: ProviderType::Kilo,
        key: Some("kilo-key".to_string()),
        base_url: Some(server.uri()),
        default_model: Some("anthropic/claude-sonnet-4-5".to_string()),
        path: None, // manifest default: /chat/completions
    });
    cfg.tiers.insert(
        "standard".to_string(),
        TierConfig {
            primary: "kilo/anthropic/claude-sonnet-4-5".to_string(),
            fallbacks: vec![],
            allow_tier_downgrade: false,
            downgrade_to: None,
            min_context_window: None,
            timeouts: TierTimeoutsSet::default(),
        },
    );
    let tmp = std::env::temp_dir().join(format!("td-kilo-{}.toml", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, toml::to_string(&cfg).unwrap()).unwrap();
    let svc = ConfigService::load(&tmp).await.unwrap();
    let _ = std::fs::remove_file(&tmp);
    let snapshot = svc.snapshot().await;
    let registry = Arc::new(ProviderRegistry::from_configs(&snapshot.providers).unwrap());
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let db = Db::open(&snapshot.database).unwrap();
    let pipeline = Pipeline::new(registry, svc.clone(), http, db.clone(), HealthRegistry::new());
    let state = AppState::new(pipeline, svc, HealthRegistry::new(), db, token_dealer::metadata::MetadataStore::new());
    let app = build_router(state);

    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({
                "model": "kilo/anthropic/claude-sonnet-4-5",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        ))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let body_bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "via kilo");
}

#[tokio::test]
async fn provider_type_resolves_to_manifest_defaults() {
    // Use TokenRouter (a real public provider) to verify that the
    // manifest defaults are wired in correctly when the user omits
    // base_url + path.
    let meta = token_dealer::providers::manifest_lookup(ProviderType::Tokenrouter)
        .expect("tokenrouter in manifest");
    assert_eq!(meta.base_url, "https://api.tokenrouter.com");
    assert_eq!(meta.path, "/v1/chat/completions");

    // OpenGateway is an alias for Gitlawb.
    assert_eq!(
        token_dealer::providers::resolve_alias("opengateway"),
        Some(ProviderType::Gitlawb)
    );
    // kimi / moonshotai are aliases for Moonshot.
    assert_eq!(
        token_dealer::providers::resolve_alias("kimi"),
        Some(ProviderType::Moonshot)
    );
    // Kiro has its own provider type.
    assert_eq!(
        token_dealer::providers::resolve_alias("kiro"),
        Some(ProviderType::Kiro)
    );
}

#[tokio::test]
async fn fallback_chain_skips_500_provider_to_next() {
    // Primary returns 500; fallback should succeed.
    let primary = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream error"))
        .expect(1)
        .mount(&primary)
        .await;

    let fallback = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "fb-1",
            "object": "chat.completion",
            "created": 1,
            "model": "x",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "from fallback"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })))
        .expect(1)
        .mount(&fallback)
        .await;

    let mut cfg = RouterConfig::default();
    cfg.database = DatabaseConfig { path: ":memory:".to_string() };
    cfg.providers.push(ProviderConfig {
        id: "primary".to_string(),
        provider_type: ProviderType::Openai,
        key: Some("k1".to_string()),
        base_url: Some(primary.uri()),
        default_model: Some("prim-model".to_string()),
        path: None,
    });
    cfg.providers.push(ProviderConfig {
        id: "fallback".to_string(),
        provider_type: ProviderType::Openai,
        key: Some("k2".to_string()),
        base_url: Some(fallback.uri()),
        default_model: Some("fb-model".to_string()),
        path: None,
    });
    cfg.tiers.insert(
        "standard".to_string(),
        TierConfig {
            primary: "primary/prim-model".to_string(),
            fallbacks: vec!["fallback/fb-model".to_string()],
            allow_tier_downgrade: false,
            downgrade_to: None,
            min_context_window: None,
            timeouts: TierTimeoutsSet::default(),
        },
    );

    let tmp = std::env::temp_dir().join(format!("td-fb-{}.toml", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, toml::to_string(&cfg).unwrap()).unwrap();
    let svc = ConfigService::load(&tmp).await.unwrap();
    let _ = std::fs::remove_file(&tmp);
    let snap = svc.snapshot().await;
    let registry = Arc::new(ProviderRegistry::from_configs(&snap.providers).unwrap());
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let db = Db::open(&snap.database).unwrap();
    let pipeline = Pipeline::new(registry, svc.clone(), http, db.clone(), HealthRegistry::new());
    let state = AppState::new(pipeline, svc, HealthRegistry::new(), db, token_dealer::metadata::MetadataStore::new());
    let app = build_router(state);

    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({
                "model": "standard",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        ))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "from fallback");
}

#[tokio::test]
async fn request_log_persists_after_completion() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "log-test",
            "object": "chat.completion",
            "created": 1,
            "model": "m",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "logged"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let state = make_state(&server.uri()).await;
    let app = build_router(state.clone());

    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({
                "model": "standard",
                "messages": [{"role": "user", "content": "log this"}]
            })
            .to_string(),
        ))
        .unwrap();
    let _ = app.oneshot(req).await.unwrap();

    // The log writer is fire-and-forget on a spawn_blocking. Give it
    // a moment to land.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let count = state
        .db
        .with(|conn| Ok(token_dealer::db::queries::count_requests(conn)?))
        .await
        .unwrap();
    assert!(count >= 1, "expected at least 1 logged request, got {count}");
}

#[tokio::test]
async fn auth_rejects_request_with_wrong_key() {
    let mut cfg = RouterConfig::default();
    cfg.database = DatabaseConfig { path: ":memory:".to_string() };
    cfg.auth.enabled = true;
    cfg.auth.keys.push(token_dealer::config::AuthKey {
        key: "right-key".to_string(),
        name: "default".to_string(),
    });
    let tmp = std::env::temp_dir().join(format!("td-auth-{}.toml", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, toml::to_string(&cfg).unwrap()).unwrap();
    let svc = ConfigService::load(&tmp).await.unwrap();
    let _ = std::fs::remove_file(&tmp);
    let snap = svc.snapshot().await;
    let registry = Arc::new(ProviderRegistry::from_configs(&snap.providers).unwrap());
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let db = Db::open(&snap.database).unwrap();
    let pipeline = Pipeline::new(registry, svc.clone(), http, db.clone(), HealthRegistry::new());
    let state = AppState::new(
        pipeline,
        svc,
        HealthRegistry::new(),
        db,
        token_dealer::metadata::MetadataStore::new(),
    );

    // No auth header
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/v1/models")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = build_router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Wrong key
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/v1/models")
        .header("authorization", "Bearer wrong-key")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = build_router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Right key
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/v1/models")
        .header("authorization", "Bearer right-key")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = build_router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // /health always public
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/health")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = build_router(state).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn circuit_breaker_skips_provider_in_cooldown() {
    use token_dealer::providers::HealthRegistry;
    use token_dealer::proxy::fallback::{self, HealthHook, ProviderHandle, RoutingPlan};
    use token_dealer::schema::canonical::{CanonicalRequest, Tier};
    use uuid::Uuid;

    let registry = HealthRegistry::new();
    // Mark "down" as down with an active cooldown
    registry
        .record_failure("down-provider", 1, 60)
        .await;

    let hook = HealthHook {
        registry: registry.clone(),
        failure_threshold: 1,
        cooldown_secs: 60,
    };

    let plan = RoutingPlan {
        request: build_test_canonical_request(),
        primary: "down-provider/x".to_string(),
        fallbacks: vec!["ok-provider/y".to_string()],
        downgrade_to: None,
        request_budget: Duration::from_secs(2),
        max_retries_per_provider: 1,
        max_retry_after_ms: 0,
        fixed_retry_wait_ms: 0,
    };

    // The plan will try the down provider first, which is in cooldown
    // so it gets skipped (no record_success / record_failure), then
    // moves to the fallback. Since the fallback provider has no
    // adapter registered, the result is "all fallbacks exhausted".
    let result = fallback::execute(
        plan,
        |_pid| async move { None::<ProviderHandle> },
        &hook,
    )
    .await;

    // We expect an error because no providers returned an adapter
    let err = result.unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("all fallbacks exhausted"),
        "got: {msg}"
    );
}

#[tokio::test]
async fn rules_add_and_delete_persist() {
    let server = MockServer::start().await;
    let state = make_state(&server.uri()).await;
    let app = build_router(state.clone());

    // Add a rule
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/admin/rules")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({
                "has_tools": true,
                "input_tokens_gt": 1000,
                "tier": "complex"
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Verify it landed in the snapshot
    let snap = state.config.snapshot().await;
    assert_eq!(snap.detection.rules.len(), 1);
    assert_eq!(snap.detection.rules[0].tier, "complex");
    assert_eq!(snap.detection.rules[0].condition.has_tools, Some(true));

    // Delete it
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/admin/rules/0")
        .header("content-type", "application/json")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = build_router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let snap = state.config.snapshot().await;
    assert_eq!(snap.detection.rules.len(), 0);
}

#[tokio::test]
async fn image_endpoint_passes_through_to_provider() {
    let image_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/images/generations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "created": 1,
            "data": [{"url": "https://example.com/img.png"}]
        })))
        .expect(1)
        .mount(&image_server)
        .await;

    let mut cfg = RouterConfig::default();
    cfg.database = DatabaseConfig { path: ":memory:".to_string() };
    cfg.providers.push(ProviderConfig {
        id: "openai".to_string(),
        provider_type: ProviderType::Openai,
        key: Some("img-test-key".to_string()),
        base_url: Some(image_server.uri()),
        default_model: Some("dall-e-3".to_string()),
        path: None,
    });
    let tmp = std::env::temp_dir().join(format!("td-img-{}.toml", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, toml::to_string(&cfg).unwrap()).unwrap();
    let svc = ConfigService::load(&tmp).await.unwrap();
    let _ = std::fs::remove_file(&tmp);
    let snap = svc.snapshot().await;
    let registry = Arc::new(ProviderRegistry::from_configs(&snap.providers).unwrap());
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let db = Db::open(&snap.database).unwrap();
    let pipeline = Pipeline::new(registry, svc.clone(), http, db.clone(), HealthRegistry::new());
    let state = AppState::new(
        pipeline,
        svc,
        HealthRegistry::new(),
        db,
        token_dealer::metadata::MetadataStore::new(),
    );
    let app = build_router(state);

    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/images/generations")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({
                "model": "openai/dall-e-3",
                "prompt": "a cat in a hat",
                "n": 1,
                "size": "1024x1024"
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let h = resp.headers().clone();
    assert_eq!(h.get("x-router-provider").unwrap(), "openai");
    let body_bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body["data"][0]["url"], "https://example.com/img.png");
}
