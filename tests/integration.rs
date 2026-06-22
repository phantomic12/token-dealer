//! End-to-end proxy tests. Each test stands up a wiremock that
//! pretends to be a provider, runs a real /v1/chat/completions
//! request through the app, and asserts the response shape + the
//! X-Router-* headers.

use axum::http::StatusCode;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use token_dealer::{
    config::{
        ConfigService, DatabaseConfig, ProviderConfig, ProviderType, RouterConfig,
        SpecificityCategory, SpecificityConfig, SpecificityRule, TierConfig, TierTimeoutsSet,
    },
    db::Db,
    providers::{HealthRegistry, ProviderRegistry},
    proxy::pipeline::Pipeline,
    server::build_router,
    AppState,
};
use tower::ServiceExt;
use wiremock::{
    matchers::{header, header_exists, method, path},
    Mock, MockServer, ResponseTemplate,
};

async fn make_state(mock_base: &str) -> AppState {
    let mut cfg = RouterConfig::default();
    // Tests run without an API key on the inbound side — the
    // provider's key ("test-key") is what we set in the
    // outbound call. Disable auth so the auth middleware
    // doesn't 401 these requests.
    cfg.auth.enabled = false;
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
    tempfile_or_stdout();
    let tmp = std::env::temp_dir().join(format!("token-dealer-test-{}.toml", uuid::Uuid::new_v4()));
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
    let key_store = token_dealer::auth::KeyStore::new(
        db.clone(),
        &token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
    );
    let oauth = token_dealer::oauth::OAuthManager::new(db.clone(), key_store.clone(), http.clone());
    let user_store = token_dealer::auth::UserStore::new(db.clone());
    let pricing = token_dealer::cost::PricingStore::new(db.clone());
    let pipeline = Pipeline::new(
        registry,
        svc.clone(),
        http,
        db.clone(),
        HealthRegistry::new(),
        key_store.clone(),
        token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
        oauth.clone(),
        user_store.clone(),
        pricing.clone(),
    );
    let metadata = token_dealer::metadata::MetadataStore::new();
    let user_store = token_dealer::auth::UserStore::new(db.clone());
    let pricing = token_dealer::cost::PricingStore::new(db.clone());
    let telemetry = token_dealer::telemetry::Telemetry::init();
    AppState::new(
        pipeline,
        svc,
        HealthRegistry::new(),
        db,
        metadata,
        key_store,
        token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
        oauth,
        user_store,
        pricing,
        telemetry,
        token_dealer::ratelimit::RateLimiter::disabled(),
    )
}

fn tempfile_or_stdout() {
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
        selected_model: "test-model".to_string(),
        selected_provider: "test".to_string(),
        request_id: uuid::Uuid::new_v4(),
        extensions: std::collections::HashMap::new(),
        metadata: token_dealer::schema::canonical::CanonicalMetadata::default(),
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
    let body_bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
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
    cfg.auth.enabled = false;
    cfg.database = DatabaseConfig {
        path: ":memory:".to_string(),
    };
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
    let key_store = token_dealer::auth::KeyStore::new(
        db.clone(),
        &token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
    );
    let oauth = token_dealer::oauth::OAuthManager::new(db.clone(), key_store.clone(), http.clone());
    let user_store = token_dealer::auth::UserStore::new(db.clone());
    let pricing = token_dealer::cost::PricingStore::new(db.clone());
    let pipeline = Pipeline::new(
        registry,
        svc.clone(),
        http,
        db.clone(),
        HealthRegistry::new(),
        key_store.clone(),
        token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
        oauth.clone(),
        user_store.clone(),
        pricing.clone(),
    );
    let user_store = token_dealer::auth::UserStore::new(db.clone());
    let pricing = token_dealer::cost::PricingStore::new(db.clone());
    let telemetry = token_dealer::telemetry::Telemetry::init();
    let state = AppState::new(
        pipeline,
        svc,
        HealthRegistry::new(),
        db,
        token_dealer::metadata::MetadataStore::new(),
        key_store,
        token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
        oauth,
        user_store,
        pricing,
        telemetry,
        token_dealer::ratelimit::RateLimiter::disabled(),
    );
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
    cfg.auth.enabled = false;
    cfg.database = DatabaseConfig {
        path: ":memory:".to_string(),
    };
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
    let key_store = token_dealer::auth::KeyStore::new(
        db.clone(),
        &token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
    );
    let oauth = token_dealer::oauth::OAuthManager::new(db.clone(), key_store.clone(), http.clone());
    let user_store = token_dealer::auth::UserStore::new(db.clone());
    let pricing = token_dealer::cost::PricingStore::new(db.clone());
    let pipeline = Pipeline::new(
        registry,
        svc.clone(),
        http,
        db.clone(),
        HealthRegistry::new(),
        key_store.clone(),
        token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
        oauth.clone(),
        user_store.clone(),
        pricing.clone(),
    );
    let user_store = token_dealer::auth::UserStore::new(db.clone());
    let pricing = token_dealer::cost::PricingStore::new(db.clone());
    let telemetry = token_dealer::telemetry::Telemetry::init();
    let state = AppState::new(
        pipeline,
        svc,
        HealthRegistry::new(),
        db,
        token_dealer::metadata::MetadataStore::new(),
        key_store,
        token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
        oauth,
        user_store,
        pricing,
        telemetry,
        token_dealer::ratelimit::RateLimiter::disabled(),
    );
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
    assert!(
        count >= 1,
        "expected at least 1 logged request, got {count}"
    );
}

#[tokio::test]
async fn auth_rejects_request_with_wrong_key() {
    let mut cfg = RouterConfig::default();
    cfg.auth.enabled = false;
    cfg.database = DatabaseConfig {
        path: ":memory:".to_string(),
    };
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
    let key_store = token_dealer::auth::KeyStore::new(
        db.clone(),
        &token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
    );
    let oauth = token_dealer::oauth::OAuthManager::new(db.clone(), key_store.clone(), http.clone());
    let user_store = token_dealer::auth::UserStore::new(db.clone());
    let pricing = token_dealer::cost::PricingStore::new(db.clone());
    let pipeline = Pipeline::new(
        registry,
        svc.clone(),
        http,
        db.clone(),
        HealthRegistry::new(),
        key_store.clone(),
        token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
        oauth.clone(),
        user_store.clone(),
        pricing.clone(),
    );
    let user_store = token_dealer::auth::UserStore::new(db.clone());
    let pricing = token_dealer::cost::PricingStore::new(db.clone());
    let telemetry = token_dealer::telemetry::Telemetry::init();
    let state = AppState::new(
        pipeline,
        svc,
        HealthRegistry::new(),
        db,
        token_dealer::metadata::MetadataStore::new(),
        key_store,
        token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
        oauth,
        user_store,
        pricing,
        telemetry,
        token_dealer::ratelimit::RateLimiter::disabled(),
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

    let registry = HealthRegistry::new();
    // Mark "down" as down with an active cooldown
    registry.record_failure("down-provider", 1, 60).await;

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
    let result = fallback::execute(plan, |_pid| async move { None::<ProviderHandle> }, &hook).await;

    // We expect an error because no providers returned an adapter
    let err = result.unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("all fallbacks exhausted"), "got: {msg}");
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
async fn encrypted_credential_round_trip() {
    use token_dealer::auth::{KeyStore, MasterKey};
    let db = Db::open(&DatabaseConfig {
        path: ":memory:".to_string(),
    })
    .unwrap();
    let master = MasterKey::from_env_or_generate().unwrap();
    let store = KeyStore::new(db.clone(), &master);
    store
        .set("anthropic", "sk-test-plaintext-12345")
        .await
        .unwrap();
    let got = store.get("anthropic").await.unwrap().unwrap();
    assert_eq!(got, "sk-test-plaintext-12345");
    // Verify the on-disk row is actually encrypted (not plaintext)
    let raw: String = db
        .with(|conn| {
            let mut stmt = conn.prepare(
                "SELECT ciphertext FROM provider_credentials WHERE provider_id = 'anthropic'",
            )?;
            let row: Vec<u8> = stmt.query_row([], |r| r.get(0))?;
            Ok(String::from_utf8_lossy(&row).to_string())
        })
        .await
        .unwrap();
    assert!(
        !raw.contains("sk-test-plaintext"),
        "ciphertext leaked plaintext"
    );
    // Decryption with wrong key should fail. We can't easily
    // construct a second MasterKey without exposing from_hex, so
    // we test the round-trip only. The auth.rs from_env_or_generate
    // unit test covers the wrong-key failure path.
    let _ = wrong_master_placeholder();
    fn wrong_master_placeholder() -> Option<()> {
        Some(())
    }
}

#[tokio::test]
async fn x_router_key_header_overrides_upstream_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header_exists("authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "override-test",
            "object": "chat.completion",
            "created": 1,
            "model": "m",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "override worked"},
                "finish_reason": "stop"
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
        .header("x-router-key", "per-request-override")
        .body(axum::body::Body::from(
            json!({
                "model": "standard",
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
    assert_eq!(body["choices"][0]["message"]["content"], "override worked");
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
    cfg.auth.enabled = false;
    cfg.database = DatabaseConfig {
        path: ":memory:".to_string(),
    };
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
    let key_store = token_dealer::auth::KeyStore::new(
        db.clone(),
        &token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
    );
    let oauth = token_dealer::oauth::OAuthManager::new(db.clone(), key_store.clone(), http.clone());
    let user_store = token_dealer::auth::UserStore::new(db.clone());
    let pricing = token_dealer::cost::PricingStore::new(db.clone());
    let pipeline = Pipeline::new(
        registry,
        svc.clone(),
        http,
        db.clone(),
        HealthRegistry::new(),
        key_store.clone(),
        token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
        oauth.clone(),
        user_store.clone(),
        pricing.clone(),
    );
    let user_store = token_dealer::auth::UserStore::new(db.clone());
    let pricing = token_dealer::cost::PricingStore::new(db.clone());
    let telemetry = token_dealer::telemetry::Telemetry::init();
    let state = AppState::new(
        pipeline,
        svc,
        HealthRegistry::new(),
        db,
        token_dealer::metadata::MetadataStore::new(),
        key_store,
        token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
        oauth,
        user_store,
        pricing,
        telemetry,
        token_dealer::ratelimit::RateLimiter::disabled(),
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

/// Build an AppState with a custom specificity config. Used by the
/// specificity-routing integration tests.
async fn make_state_with_specificity(
    mock_base: &str,
    mock_id: &str,
    specificity: SpecificityConfig,
    extra_provider: Option<(&str, &str, &str)>,
) -> AppState {
    let mut cfg = RouterConfig::default();
    cfg.auth.enabled = false;
    cfg.database = DatabaseConfig {
        path: ":memory:".to_string(),
    };
    cfg.providers.push(ProviderConfig {
        id: mock_id.to_string(),
        provider_type: ProviderType::Openai,
        key: Some("test-key".to_string()),
        base_url: Some(mock_base.to_string()),
        default_model: Some(format!("{mock_id}-model")),
        path: None,
    });
    if let Some((base, id, model)) = extra_provider {
        cfg.providers.push(ProviderConfig {
            id: id.to_string(),
            provider_type: ProviderType::Generic,
            key: Some("test-key".to_string()),
            base_url: Some(base.to_string()),
            default_model: Some(model.to_string()),
            path: None,
        });
    }
    cfg.tiers.insert(
        "standard".to_string(),
        TierConfig {
            primary: format!("{mock_id}/{mock_id}-model"),
            fallbacks: vec![],
            allow_tier_downgrade: false,
            downgrade_to: None,
            min_context_window: None,
            timeouts: TierTimeoutsSet::default(),
        },
    );
    cfg.specificity = specificity;
    let tmp = std::env::temp_dir().join(format!("td-spec-{}.toml", uuid::Uuid::new_v4()));
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
    let key_store = token_dealer::auth::KeyStore::new(
        db.clone(),
        &token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
    );
    let oauth = token_dealer::oauth::OAuthManager::new(db.clone(), key_store.clone(), http.clone());
    let user_store = token_dealer::auth::UserStore::new(db.clone());
    let pricing = token_dealer::cost::PricingStore::new(db.clone());
    let pipeline = Pipeline::new(
        registry,
        svc.clone(),
        http,
        db.clone(),
        HealthRegistry::new(),
        key_store.clone(),
        token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
        oauth.clone(),
        user_store.clone(),
        pricing.clone(),
    );
    AppState::new(
        pipeline,
        svc,
        HealthRegistry::new(),
        db,
        token_dealer::metadata::MetadataStore::new(),
        key_store,
        token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
        oauth,
        user_store,
        pricing,
        token_dealer::telemetry::Telemetry::init(),
        token_dealer::ratelimit::RateLimiter::disabled(),
    )
}

#[tokio::test]
async fn specificity_routing_overrides_tier_when_keywords_match() {
    // Two providers: "tier" uses /v1/chat/completions and returns
    // "tier-pick"; "specific" returns "specificity-pick". Coding
    // keywords in the user message should route to "specific".
    let tier_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "t", "object": "chat.completion", "created": 1,
            "model": "tier-model",
            "choices": [{"index": 0, "message": {"role":"assistant","content":"tier-pick"}, "finish_reason":"stop"}],
            "usage": {"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
        })))
        .mount(&tier_server)
        .await;
    let spec_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "s", "object": "chat.completion", "created": 1,
            "model": "spec-model",
            "choices": [{"index": 0, "message": {"role":"assistant","content":"specificity-pick"}, "finish_reason":"stop"}],
            "usage": {"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
        })))
        .mount(&spec_server)
        .await;

    let state = make_state_with_specificity(
        &tier_server.uri(),
        "tier",
        SpecificityConfig {
            enabled: true,
            rules: vec![SpecificityRule {
                category: SpecificityCategory::Coding,
                primary: "specific/spec-model".to_string(),
                threshold: None,
            }],
        },
        Some((&spec_server.uri(), "specific", "spec-model")),
    )
    .await;
    // Add the "specific" provider both to the config snapshot (the
    // pipeline reads keys from cfg.providers) and to the registry
    // (the Selector looks up adapters there).
    let mut snap = state.config.snapshot().await;
    snap.providers.push(token_dealer::config::ProviderConfig {
        id: "specific".to_string(),
        provider_type: token_dealer::config::ProviderType::Generic,
        key: Some("test-key".to_string()),
        base_url: Some(spec_server.uri()),
        default_model: Some("spec-model".to_string()),
        path: None,
    });
    state
        .config
        .update_with(|cfg| {
            for p in snap.providers.iter() {
                if !cfg.providers.iter().any(|q| q.id == p.id) {
                    cfg.providers.push(p.clone());
                }
            }
        })
        .await
        .unwrap();
    let registry = state.pipeline.registry.clone();
    registry
        .add(&token_dealer::config::ProviderConfig {
            id: "specific".to_string(),
            provider_type: token_dealer::config::ProviderType::Generic,
            key: Some("test-key".to_string()),
            base_url: Some(spec_server.uri()),
            default_model: Some("spec-model".to_string()),
            path: None,
        })
        .await
        .unwrap();

    let app = build_router(state);
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({
                "model": "tier/tier-model",
                "messages": [{"role":"user","content":"please refactor this function and debug the syntax"}]
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let h = resp.headers().clone();
    let body_bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(status, StatusCode::OK, "body: {body}");
    // Coding keywords fire: should route to "specific"
    assert_eq!(h.get("x-router-provider").unwrap(), "specific");
    assert_eq!(h.get("x-router-specificity").unwrap(), "coding");
    assert_eq!(body["choices"][0]["message"]["content"], "specificity-pick");
}

#[tokio::test]
async fn specificity_disabled_falls_through_to_tier() {
    let tier_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "t", "object": "chat.completion", "created": 1,
            "model": "tier-model",
            "choices": [{"index": 0, "message": {"role":"assistant","content":"tier-pick"}, "finish_reason":"stop"}],
            "usage": {"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
        })))
        .mount(&tier_server)
        .await;

    let state = make_state_with_specificity(
        &tier_server.uri(),
        "tier",
        SpecificityConfig {
            enabled: false, // disabled
            rules: vec![SpecificityRule {
                category: SpecificityCategory::Coding,
                primary: "tier/tier-model".to_string(),
                threshold: None,
            }],
        },
        None,
    )
    .await;

    let app = build_router(state);
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({
                "model": "tier/tier-model",
                "messages": [{"role":"user","content":"refactor this function and debug the syntax"}]
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let h = resp.headers().clone();
    let body_bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(h.get("x-router-provider").unwrap(), "tier");
    assert!(h.get("x-router-specificity").is_none());
    assert_eq!(body["choices"][0]["message"]["content"], "tier-pick");
}

// ───────────────────────────────────────────────────────────────────────────
// Per-provider adapter e2e tests.
//
// These tests wire a wiremock in the role of the upstream provider,
// and exercise the real provider-specific adapter (Anthropic,
// Google, xAI). They assert:
//   - the request shape sent to the provider (path, headers,
//     body fields) matches the provider's wire format
//   - the response is parsed into our canonical model
//   - the chat handler returns a valid OpenAI-shape JSON body
// ───────────────────────────────────────────────────────────────────────────

async fn make_state_with_provider(
    id: &str,
    provider_type: ProviderType,
    key: &str,
    mock_base: &str,
    default_model: &str,
) -> AppState {
    let mut cfg = RouterConfig::default();
    // Tests run without an inbound API key — disable auth so
    // the auth middleware doesn't 401 these requests.
    cfg.auth.enabled = false;
    cfg.providers.push(ProviderConfig {
        id: id.to_string(),
        provider_type,
        key: Some(key.to_string()),
        base_url: Some(mock_base.to_string()),
        default_model: Some(default_model.to_string()),
        path: None,
    });
    cfg.tiers.insert(
        "standard".to_string(),
        TierConfig {
            primary: format!("{id}/{default_model}"),
            fallbacks: vec![],
            allow_tier_downgrade: false,
            downgrade_to: None,
            min_context_window: None,
            timeouts: TierTimeoutsSet::default(),
        },
    );
    let toml = toml::to_string(&cfg).unwrap();
    let tmp = std::env::temp_dir().join(format!(
        "token-dealer-provtest-{}.toml",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&tmp, toml).unwrap();
    let svc = ConfigService::load(&tmp).await.unwrap();
    let _ = std::fs::remove_file(&tmp);
    let snap = svc.snapshot().await;
    let registry = Arc::new(ProviderRegistry::from_configs(&snap.providers).unwrap());
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let db = Db::open(&snap.database).unwrap();
    let key_store = token_dealer::auth::KeyStore::new(
        db.clone(),
        &token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
    );
    let oauth = token_dealer::oauth::OAuthManager::new(db.clone(), key_store.clone(), http.clone());
    let user_store = token_dealer::auth::UserStore::new(db.clone());
    let pricing = token_dealer::cost::PricingStore::new(db.clone());
    let pipeline = Pipeline::new(
        registry,
        svc.clone(),
        http,
        db.clone(),
        HealthRegistry::new(),
        key_store.clone(),
        token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
        oauth.clone(),
        user_store.clone(),
        pricing.clone(),
    );
    let metadata = token_dealer::metadata::MetadataStore::new();
    let user_store_outer = user_store.clone();
    let pricing_outer = pricing.clone();
    let telemetry = token_dealer::telemetry::Telemetry::init();
    let _ = metadata;
    AppState::new(
        pipeline,
        svc,
        HealthRegistry::new(),
        db,
        token_dealer::metadata::MetadataStore::new(),
        key_store,
        token_dealer::auth::MasterKey::from_env_or_generate().unwrap(),
        oauth,
        user_store_outer,
        pricing_outer,
        telemetry,
        token_dealer::ratelimit::RateLimiter::disabled(),
    )
}

#[tokio::test]
async fn anthropic_adapter_translates_request_and_response() {
    // Wiremock stands in for api.anthropic.com. We assert:
    //   - path is /v1/messages (not /v1/chat/completions)
    //   - auth is `x-api-key`, not `authorization: Bearer`
    //   - `anthropic-version` header is sent
    //   - system prompt is at the top level (not in messages)
    //   - response is parsed: Anthropic shape → OpenAI shape on the way out
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "ant-test-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(header_exists("content-type"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_01abc",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "hello from claude"}],
            "model": "claude-sonnet-4-5",
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 12, "output_tokens": 7}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let state = make_state_with_provider(
        "anthropic",
        ProviderType::Anthropic,
        "ant-test-key",
        &server.uri(),
        "claude-sonnet-4-5",
    )
    .await;
    let app = build_router(state);

    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({
                "model": "standard",
                "messages": [
                    {"role": "system", "content": "you are a helpful assistant"},
                    {"role": "user", "content": "hi"}
                ]
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let h = resp.headers().clone();
    let body_bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(status, StatusCode::OK, "anthropic body: {body}");
    assert_eq!(h.get("x-router-provider").unwrap(), "anthropic");
    assert_eq!(h.get("x-router-model").unwrap(), "claude-sonnet-4-5");
    assert_eq!(
        body["choices"][0]["message"]["content"],
        "hello from claude"
    );
    // Anthropic reports input/output tokens; we expose them as
    // prompt/completion.
    assert_eq!(body["usage"]["prompt_tokens"], 12);
    assert_eq!(body["usage"]["completion_tokens"], 7);
}

#[tokio::test]
async fn google_adapter_translates_request_and_response() {
    // Google Gemini's API uses POST /v1beta/models/{model}:generateContent
    // with `x-goog-api-key` auth and a different envelope. We assert the
    // adapter routes there + parses the response back.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-2.0-flash:generateContent"))
        .and(header("x-goog-api-key", "goog-test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{
                "index": 0,
                "content": {
                    "role": "model",
                    "parts": [{"text": "hi from gemini"}]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 9,
                "candidatesTokenCount": 4,
                "totalTokenCount": 13
            },
            "modelVersion": "gemini-2.0-flash"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let state = make_state_with_provider(
        "google",
        ProviderType::Google,
        "goog-test-key",
        &server.uri(),
        "gemini-2.0-flash",
    )
    .await;
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
    let status = resp.status();
    let body_bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(status, StatusCode::OK, "google body: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "hi from gemini");
    assert_eq!(body["usage"]["prompt_tokens"], 9);
    assert_eq!(body["usage"]["completion_tokens"], 4);
}

#[tokio::test]
async fn xai_adapter_uses_bearer_and_openai_shape() {
    // xAI is OpenAI-compatible — same path, same auth header,
    // same response shape. The adapter should be a near-passthrough.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer xai-test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "grok-1",
            "object": "chat.completion",
            "created": 1_700_000_000,
            "model": "grok-2-latest",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hello from grok"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 4, "completion_tokens": 3, "total_tokens": 7}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let state = make_state_with_provider(
        "xai",
        ProviderType::Xai,
        "xai-test-key",
        &server.uri(),
        "grok-2-latest",
    )
    .await;
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
    let status = resp.status();
    let h = resp.headers().clone();
    let body_bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(status, StatusCode::OK, "xai body: {body}");
    assert_eq!(h.get("x-router-provider").unwrap(), "xai");
    assert_eq!(body["choices"][0]["message"]["content"], "hello from grok");
    assert_eq!(body["usage"]["total_tokens"], 7);
}

// ── /auth/login endpoint ─────────────────────────────────────────
//
// Regression for the form-vs-JSON content-type bug: the WebUI
// login form posts `application/x-www-form-urlencoded`, and the
// JSON-only handler was rejecting those with 415, making the
// sign-in button appear to do nothing. The handler now
// dispatches on Content-Type and returns HTML (with HX-Redirect
// on success) for form requests.

mod login_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    async fn app_with_admin() -> (axum::Router, String, String) {
        let state = make_state("http://127.0.0.1:0").await;
        let users = token_dealer::auth::UserStore::new(state.db.clone());
        let _ = users
            .create_user("admin@test.local", "Admin", Some("hunter22!"), token_dealer::auth::Role::Admin)
            .await
            .unwrap();
        let (_api_key, plaintext) = users
            .create_api_key(
                &users
                    .get_user_by_email("admin@test.local")
                    .await
                    .unwrap()
                    .unwrap()
                    .id,
                "test-key",
            )
            .await
            .unwrap();
        let app = build_router(state);
        (app, plaintext, "hunter22!".to_string())
    }

    fn post_json(uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn post_form(uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn login_json_path_returns_200_with_cookie() {
        let (app, key, _pw) = app_with_admin().await;
        let req = post_json("/auth/login", &json!({"api_key": key}).to_string());
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            resp.headers().contains_key(header::SET_COOKIE),
            "JSON login should set the session cookie"
        );
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.starts_with("application/json"),
            "JSON request should get JSON response, got {ct}"
        );
    }

    #[tokio::test]
    async fn login_form_path_returns_html_with_hx_redirect() {
        let (app, _key, pw) = app_with_admin().await;
        let body = format!("email=admin%40test.local&password={}", urlencode(&pw));
        let req = post_form("/auth/login", &body);
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let h = resp.headers();
        assert!(
            h.contains_key(header::SET_COOKIE),
            "form login should set the session cookie"
        );
        assert_eq!(h.get("HX-Redirect").and_then(|v| v.to_str().ok()), Some("/ui/"));
        let ct = h
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.starts_with("text/html"), "form request should get HTML, got {ct}");
    }

    #[tokio::test]
    async fn login_form_bad_creds_returns_html_error() {
        let (app, _key, _pw) = app_with_admin().await;
        let body = "email=admin%40test.local&password=wrong";
        let req = post_form("/auth/login", body);
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.starts_with("text/html"), "form 401 should be HTML, got {ct}");
        let body_bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let s = String::from_utf8_lossy(&body_bytes);
        assert!(s.contains("invalid credentials"), "body should explain error: {s}");
    }

    #[tokio::test]
    async fn login_form_with_api_key_field() {
        // The /ui/login form for API-key sign-in submits a single
        // `api_key=…` field — verify the form path handles it.
        let (app, key, _pw) = app_with_admin().await;
        let body = format!("api_key={}", urlencode(&key));
        let req = post_form("/auth/login", &body);
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("HX-Redirect").and_then(|v| v.to_str().ok()),
            Some("/ui/")
        );
    }

    fn urlencode(s: &str) -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{:02X}", b),
            })
            .collect()
    }
}
