//! Admin endpoints. JSON + form-encoded.
//! `POST /admin/providers` adds a provider in-memory and (via the UI's
//! form flow) the change is persisted to TOML through `update_with`.
//! `DELETE /admin/providers/:id` removes a provider.
//! `POST /admin/tiers/:tier` updates a tier's primary + fallbacks + timeouts.
//! `POST /admin/config/save` forces a disk flush.

use super::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;

use crate::config::types::{
    DetectionCondition, DetectionRule, ProviderConfig, ProviderType, TierConfig,
};

pub async fn add_provider(
    State(state): State<AppState>,
    Json(body): Json<ProviderConfig>,
) -> Response {
    // Validate type string. serde already does this for typed enums;
    // an unknown variant would have been caught by the JSON parser.
    let _ = body.provider_type;

    let result = state
        .config
        .update_with(|cfg| {
            // Remove any existing entry with the same id, then append.
            cfg.providers.retain(|p| p.id != body.id);
            cfg.providers.push(body.clone());
        })
        .await;

    match result {
        Ok(_) => match state.pipeline.registry.add(&body).await {
            Ok(_) => (
                StatusCode::CREATED,
                Json(json!({"status": "added", "id": body.id})),
            )
                .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response(),
        },
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("save failed: {e}")})),
        )
            .into_response(),
    }
}

pub async fn remove_provider(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    if id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "id required"})),
        )
            .into_response();
    }
    let result = state
        .config
        .update_with(|cfg| {
            cfg.providers.retain(|p| p.id != id);
        })
        .await;
    match result {
        Ok(_) => {
            let removed = state.pipeline.registry.remove(&id).await;
            if removed {
                (StatusCode::OK, Json(json!({"status": "removed", "id": id}))).into_response()
            } else {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": format!("provider {id} not found")})),
                )
                    .into_response()
            }
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("save failed: {e}")})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub struct TierUpdate {
    pub primary: String,
    #[serde(default)]
    pub fallbacks: Vec<String>,
    #[serde(default)]
    pub allow_tier_downgrade: bool,
    #[serde(default)]
    pub downgrade_to: Option<String>,
    #[serde(default)]
    pub min_context_window: Option<u32>,
    #[serde(default)]
    pub timeouts: Option<crate::config::types::TierTimeoutsSet>,
}

pub async fn update_tier(
    State(state): State<AppState>,
    Path(tier): Path<String>,
    Json(body): Json<TierUpdate>,
) -> Response {
    if !crate::schema::canonical::Tier::parse(&tier).is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("unknown tier: {tier}")})),
        )
            .into_response();
    }
    let new_tier = TierConfig {
        primary: body.primary,
        fallbacks: body.fallbacks,
        allow_tier_downgrade: body.allow_tier_downgrade,
        downgrade_to: body.downgrade_to,
        min_context_window: body.min_context_window,
        timeouts: body.timeouts.unwrap_or_default(),
    };
    let tier_clone = tier.clone();
    let result = state
        .config
        .update_with(|cfg| {
            cfg.tiers.insert(tier_clone.clone(), new_tier);
        })
        .await;
    match result {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({"status": "updated", "tier": tier})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("save failed: {e}")})),
        )
            .into_response(),
    }
}

pub async fn save_config(State(state): State<AppState>) -> Response {
    let snapshot = state.config.snapshot().await;
    match state.config.save_to_disk(&snapshot).await {
        Ok(_) => (StatusCode::OK, Json(json!({"status": "saved"}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("save failed: {e}")})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub struct SetKeyRequest {
    pub key: String,
}

pub async fn set_key(
    State(state): State<AppState>,
    axum::extract::Path(provider_id): axum::extract::Path<String>,
    Json(body): Json<SetKeyRequest>,
) -> Response {
    if body.key.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "key required"})),
        )
            .into_response();
    }
    match state.key_store.set(&provider_id, &body.key).await {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({"status": "stored", "provider": provider_id})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("store failed: {e}")})),
        )
            .into_response(),
    }
}

pub async fn delete_key(
    State(state): State<AppState>,
    axum::extract::Path(provider_id): axum::extract::Path<String>,
) -> Response {
    match state.key_store.delete(&provider_id).await {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({"status": "deleted", "provider": provider_id})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("delete failed: {e}")})),
        )
            .into_response(),
    }
}

/// Store a refresh token for an OAuth-based provider. POST
/// /admin/oauth/:provider_id/refresh with `{"refresh_token": "..."}`.
/// The pipeline will auto-refresh on next use. The manifest's OAuth
/// config (token URL + client ID) is used automatically for
/// github-copilot, responses, and kiro.
pub async fn set_oauth_refresh(
    State(state): State<AppState>,
    axum::extract::Path(provider_id): axum::extract::Path<String>,
    Json(body): Json<SetKeyRequest>,
) -> Response {
    match state.oauth.set_refresh_token(&provider_id, &body.key).await {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({"status": "refresh token stored", "provider": provider_id})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("store failed: {e}")})),
        )
            .into_response(),
    }
}

/// Start a popup_oauth flow. POST /admin/oauth/:provider_id/start
/// with optional `{"redirect_uri": "..."}`. Returns the authorize
/// URL the user should visit.
///
/// GET on the same path also works: returns a 302 redirect straight
/// to the provider's authorize URL so the user can just click a
/// link in chat / email / the docs and land on the consent screen
/// without first having to fetch a JSON response.
pub async fn start_oauth(
    State(state): State<AppState>,
    axum::extract::Path(provider_id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
    body: Option<Json<serde_json::Value>>,
) -> Response {
    // The config stores a full callback URL like
    // `http://host:port/admin/oauth/openai/callback`. Reconstruct it
    // for whichever provider the caller asked for so all four
    // providers don't share one redirect URI.
    let snap = state.config.snapshot().await;
    let top_level = snap.oauth_redirect_uri.clone().unwrap_or_default();
    let config_redirect = if !snap.server.oauth_redirect_uri.is_empty() {
        snap.server.oauth_redirect_uri.clone()
    } else {
        top_level
    };
    let explicit_redirect = body
        .as_ref()
        .and_then(|Json(b)| b.get("redirect_uri"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let redirect_uri =
        explicit_redirect.unwrap_or_else(|| rebuild_redirect_uri(&config_redirect, &provider_id));
    match state
        .oauth
        .start_popup_oauth(&provider_id, &redirect_uri)
        .await
    {
        Ok((url, _state)) => {
            // GET: redirect the browser straight to the consent
            // page. POST: return the URL as JSON so JS can open it
            // in a popup.
            let is_get = headers
                .get(axum::http::method::Method::GET.as_str())
                .is_some()
                || !headers
                    .keys()
                    .any(|k| k.as_str().eq_ignore_ascii_case("content-type"));
            // Use the actual request method instead — axum maps
            // GET to `body: None` and POST to `body: Some(_)` above.
            if body.is_none() {
                axum::response::Redirect::to(&url).into_response()
            } else {
                (
                    StatusCode::OK,
                    Json(json!({"authorize_url": url, "state": "ok"})),
                )
                    .into_response()
            }
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("start_oauth failed: {e}")})),
        )
            .into_response(),
    }
}

/// Rebuild `oauth_redirect_uri` for the given provider.
///
/// Accepts these shapes:
///   1. Bare origin like `http://host:port` → append
///      `/admin/oauth/<provider>/callback`.
///   2. Full URL with `{provider}` placeholder:
///      `http://host:port/admin/oauth/{provider}/callback` →
///      substitute the provider segment. (Most user-friendly
///      shape — survives renaming providers without config edits.)
///   3. Full URL ending in `/admin/oauth/<provider>/callback` →
///      swap the `<provider>` segment if it doesn't match.
///      (Legacy shape that the starter
///      `token-dealer.toml.example` ships.)
pub(crate) fn rebuild_redirect_uri(configured: &str, provider_id: &str) -> String {
    if configured.is_empty() {
        return format!("/admin/oauth/{provider_id}/callback");
    }
    // Shape 2: literal `{provider}` template. Substitute then
    // return as a full URL (no further rewriting needed).
    if configured.contains("{provider}") {
        return configured.replace("{provider}", provider_id);
    }
    if configured.contains("/admin/oauth/") {
        // Shape 3: re-write the provider segment.
        if let Some((prefix, _suffix)) = configured.rsplit_once("/admin/oauth/") {
            return format!("{prefix}/admin/oauth/{provider_id}/callback");
        }
    }
    if configured.ends_with("/callback") || configured.ends_with("/") {
        let trimmed = configured.trim_end_matches('/');
        return format!("{trimmed}/admin/oauth/{provider_id}/callback");
    }
    // Shape 1: bare origin.
    format!("{configured}/admin/oauth/{provider_id}/callback")
}

/// OAuth callback. GET /admin/oauth/:provider_id/callback?code=...&state=...
/// Exchanges the code for tokens, stores the refresh_token, then
/// redirects to the UI with a flash message.
pub async fn oauth_callback(
    State(state): State<AppState>,
    axum::extract::Path(provider_id): axum::extract::Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let code = params.get("code").cloned().unwrap_or_default();
    let oauth_state = params.get("state").cloned().unwrap_or_default();
    let snap = state.config.snapshot().await;
    let top_level = snap.oauth_redirect_uri.clone().unwrap_or_default();
    let config_redirect = if !snap.server.oauth_redirect_uri.is_empty() {
        snap.server.oauth_redirect_uri.clone()
    } else {
        top_level
    };
    let redirect_uri = rebuild_redirect_uri(&config_redirect, &provider_id);
    match state
        .oauth
        .complete_popup_oauth(&provider_id, &code, &oauth_state, &redirect_uri)
        .await
    {
        Ok(_) => axum::response::Redirect::to(&format!(
            "/ui/oauth/done?provider={provider_id}&status=ok"
        ))
        .into_response(),
        Err(e) => axum::response::Redirect::to(&format!(
            "/ui/oauth/done?provider={provider_id}&status=err&msg={}",
            urlencoding_simple(&e.to_string())
        ))
        .into_response(),
    }
}

/// Start a device_code flow. POST /admin/oauth/:provider_id/device/start
/// returns the user_code + verification_uri + device_code (for
/// the client to poll).
pub async fn start_device_oauth(
    State(state): State<AppState>,
    axum::extract::Path(provider_id): axum::extract::Path<String>,
) -> Response {
    match state.oauth.start_device_flow(&provider_id).await {
        Ok(info) => (StatusCode::OK, Json(info)).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("start_device failed: {e}")})),
        )
            .into_response(),
    }
}

/// Anthropic paste-code flow. POST /admin/oauth/:provider_id/paste
/// with `{"code": "<authorization_code>#<state>"}`. Stores the code as
/// the refresh_token; the refresh path will exchange it for a real
/// access token on first request.
pub async fn paste_anthropic_code(
    State(state): State<AppState>,
    axum::extract::Path(provider_id): axum::extract::Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let code = body
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if code.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "code field is required"})),
        )
            .into_response();
    }
    match state.oauth.paste_anthropic_code(&provider_id, &code).await {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({"status": "ok", "provider": provider_id})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("{e}")})),
        )
            .into_response(),
    }
}

/// CLI setup endpoint — POST /admin/oauth/:provider_id/setup with
/// `{"refresh_token": "...", "client_id"?: "...", "client_secret"?: "..."}`.
///
/// Used by `token-dealer-login` to push a refresh_token obtained
/// through the loopback / paste-code / device flows. The server
/// stores it exactly like the popup callback would have. The
/// optional client_id + client_secret pair is for flows that
/// dynamically register an OAuth client (Kiro/AWS SSO OIDC) — the
/// pair must accompany the refresh token for future refreshes.
pub async fn setup_oauth_via_cli(
    State(state): State<AppState>,
    axum::extract::Path(provider_id): axum::extract::Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let refresh = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if refresh.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "refresh_token is required"})),
        )
            .into_response();
    }
    let registered_cid = body
        .get("client_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    let registered_cs = body
        .get("client_secret")
        .and_then(|v| v.as_str())
        .map(String::from);
    let stored = match (&registered_cid, &registered_cs) {
        (Some(cid), Some(cs)) => {
            crate::oauth::serialize_stored_refresh(&refresh, Some(cid.as_str()), Some(cs.as_str()))
        }
        _ => refresh.clone(),
    };
    if let Err(e) = state
        .key_store
        .set(&format!("oauth:{provider_id}"), &stored)
        .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("store: {e}")})),
        )
            .into_response();
    }
    // Trigger an immediate refresh so we validate the new
    // refresh_token actually works against the upstream.
    if let Ok(Some(_access)) = state.oauth.refresh(&provider_id).await {
        // ok — access token cached.
    }
    (
        StatusCode::OK,
        Json(json!({
            "status": "ok",
            "provider": provider_id,
        })),
    )
        .into_response()
}

/// Poll a device_code flow. POST /admin/oauth/device/poll
/// with `{"device_code": "..."}`. Returns `{authorized: true}` on
/// success (refresh_token stored) or `{authorized: false}` on
/// pending.
pub async fn poll_device_oauth(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let device_code = body
        .get("device_code")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if device_code.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "device_code required"})),
        )
            .into_response();
    }
    match state.oauth.poll_device_flow(&device_code).await {
        Ok(true) => (StatusCode::OK, Json(json!({"authorized": true}))).into_response(),
        Ok(false) => (StatusCode::OK, Json(json!({"authorized": false}))).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": format!("poll failed: {e}")})),
        )
            .into_response(),
    }
}

fn urlencoding_simple(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[derive(Deserialize)]
pub struct AddRuleRequest {
    /// Optional index — when present, replaces the rule at that index
    /// instead of appending.
    pub index: Option<usize>,
    pub has_tools: Option<bool>,
    pub input_tokens_gt: Option<u32>,
    pub prompt_contains: Option<Vec<String>>,
    pub tier: String,
}

pub async fn add_rule(State(state): State<AppState>, Json(body): Json<AddRuleRequest>) -> Response {
    if crate::schema::canonical::Tier::parse(&body.tier).is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("unknown tier: {}", body.tier)})),
        )
            .into_response();
    }
    let rule = DetectionRule {
        condition: DetectionCondition {
            has_tools: body.has_tools,
            input_tokens_gt: body.input_tokens_gt,
            prompt_contains: body.prompt_contains,
        },
        tier: body.tier,
    };
    let idx = body.index;
    let result = state
        .config
        .update_with(|cfg| match idx {
            Some(i) if i < cfg.detection.rules.len() => {
                cfg.detection.rules[i] = rule;
            }
            _ => {
                cfg.detection.rules.push(rule);
            }
        })
        .await;
    match result {
        Ok(_) => (StatusCode::OK, Json(json!({"status": "ok"}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("save failed: {e}")})),
        )
            .into_response(),
    }
}

pub async fn delete_rule(
    State(state): State<AppState>,
    axum::extract::Path(index): axum::extract::Path<usize>,
) -> Response {
    let result = state
        .config
        .update_with(|cfg| {
            if index < cfg.detection.rules.len() {
                cfg.detection.rules.remove(index);
            }
        })
        .await;
    match result {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({"status": "deleted", "index": index})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("save failed: {e}")})),
        )
            .into_response(),
    }
}

/// Lists available `ProviderType` values + the canonical aliases the
/// UI uses to populate the dropdown.
pub async fn list_provider_types() -> Json<serde_json::Value> {
    let types: Vec<&str> = vec![
        "anthropic",
        "google",
        "kiro",
        "responses",
        "generic",
        "openai",
        "openrouter",
        "tokenrouter",
        "groq",
        "deepseek",
        "fireworks",
        "mistral",
        "xai",
        "qwen",
        "moonshot",
        "zai",
        "xiaomi",
        "minimax",
        "byteplus",
        "nvidia",
        "opencode-go",
        "opencode-zen",
        "kilo",
        "commandcode",
        "github-copilot",
        "gitlawb",
        "ollama",
        "ollama-cloud",
        "llamacpp",
        "lmstudio",
    ];
    Json(
        json!({"types": types, "aliases": serde_json::to_value(crate::providers::manifest::ALIASES).unwrap_or(json!([]))}),
    )
}

/// Verify a provider type string parses. Used by the UI form to
/// surface validation errors without round-tripping a full add.
pub async fn validate_provider_type(Json(body): Json<serde_json::Value>) -> Response {
    let s = body
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if s.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "type field required"})),
        )
            .into_response();
    }
    if crate::providers::resolve_alias(s).is_some() {
        (StatusCode::OK, Json(json!({"status": "ok"}))).into_response()
    } else {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("unknown provider type: {s}")})),
        )
            .into_response()
    }
}

/// Live model list. POST /admin/providers/list-models with the same
/// body shape as add-provider (`id`, `type`, `key`, `base_url`,
/// `path`, `default_model`). Returns `{"models": ["...", "..."]}`
/// or a 4xx with an error message.
pub async fn list_provider_models(
    State(state): State<AppState>,
    Json(body): Json<ProviderConfig>,
) -> Response {
    if body.id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "id required"})),
        )
            .into_response();
    }
    let key = match state
        .key_store
        .get(&body.id)
        .await
        .ok()
        .flatten()
        .or_else(|| body.key.clone())
    {
        Some(k) => k,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "no key configured; paste the API key in the form first"})),
            )
                .into_response();
        }
    };
    match state.pipeline.registry.build_transient(&body) {
        Ok(adapter) => match adapter.list_models(&key, &state.pipeline.http).await {
            Ok(models) => (StatusCode::OK, Json(json!({"models": models}))).into_response(),
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": format!("{e}")})),
            )
                .into_response(),
        },
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("build adapter: {e}")})),
        )
            .into_response(),
    }
}

/// Test a provider config without persisting it. POST /admin/providers/test
/// with a JSON body matching ProviderConfig. The server builds a transient
/// adapter, hits the cheapest possible endpoint (GET /v1/models for
/// OpenAI-compat, a 1-token POST /v1/messages for Anthropic), and returns
/// the result as HTML (the wizard UI uses HTMX innerHTML swap).
pub async fn test_provider(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ProviderConfig>,
) -> Response {
    use crate::auth::resolve as resolve_key;
    use crate::config::types::ProviderType;
    use axum::http::header::HeaderName;

    let is_htmx = headers
        .get(HeaderName::from_static("hx-request"))
        .map(|v| v == "true")
        .unwrap_or(false);

    if body.id.is_empty() {
        return html_test_result(is_htmx, false, "id required".to_string());
    }
    if state
        .config
        .snapshot()
        .await
        .providers
        .iter()
        .any(|p| p.id == body.id)
    {
        return html_test_result(
            is_htmx,
            false,
            format!("provider id '{}' already exists", body.id),
        );
    }
    let key = resolve_key(&state.key_store, &body.id, body.key.as_deref()).await;

    let adapter_result = state.pipeline.registry.build_transient(&body);
    let adapter = match adapter_result {
        Ok(a) => a,
        Err(e) => return html_test_result(is_htmx, false, e.to_string()),
    };
    let base_url = adapter.base_url().trim_end_matches('/').to_string();

    let req_result = match body.provider_type {
        ProviderType::Anthropic => {
            state
                .pipeline
                .http
                .post(format!("{base_url}/v1/messages"))
                .header("x-api-key", &key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&json!({
                    "model": adapter.default_model(),
                    "max_tokens": 1,
                    "messages": [{"role": "user", "content": "hi"}]
                }))
                .send()
                .await
        }
        ProviderType::Google => {
            state
                .pipeline
                .http
                .get(format!("{base_url}/v1beta/models"))
                .header("x-goog-api-key", &key)
                .send()
                .await
        }
        ProviderType::Kiro => {
            state
                .pipeline
                .http
                .get(format!("{base_url}/ping"))
                .header("authorization", format!("Bearer {key}"))
                .send()
                .await
        }
        _ => {
            state
                .pipeline
                .http
                .get(format!("{base_url}/v1/models"))
                .header("authorization", format!("Bearer {key}"))
                .send()
                .await
        }
    };

    let resp = match req_result {
        Ok(r) => r,
        Err(e) => {
            return html_test_result(is_htmx, false, format!("connection failed: {e}"));
        }
    };
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    if status.is_success() {
        let snippet = body_text.chars().take(120).collect::<String>();
        let msg = format!(
            "{} OK ({} bytes): {}",
            status.as_u16(),
            body_text.len(),
            snippet
        );
        html_test_result(is_htmx, true, msg)
    } else {
        let snippet = body_text.chars().take(200).collect::<String>();
        let msg = format!(
            "{} {}: {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or(""),
            snippet
        );
        html_test_result(is_htmx, false, msg)
    }
}

fn html_test_result(is_htmx: bool, ok: bool, message: String) -> Response {
    if is_htmx {
        let class = if ok { "ok" } else { "error" };
        (
            StatusCode::OK,
            axum::response::Html(format!(
                r##"<div class="test-result {class}">{msg}</div>"##,
                class = class,
                msg = html_escape(&message)
            )),
        )
            .into_response()
    } else if ok {
        (
            StatusCode::OK,
            Json(json!({"status": "ok", "message": message})),
        )
            .into_response()
    } else {
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({"status": "error", "message": message})),
        )
            .into_response()
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// List every model in the `model_prices` table (used by the
/// `/ui/pricing` page). Sorted by `input_per_1k` ascending so the
/// cheapest models bubble to the top — that's the page's whole
/// point.
pub async fn list_pricing(State(state): State<AppState>) -> Response {
    match state.pricing.list().await {
        Ok(rows) => (StatusCode::OK, Json(json!({"prices": rows}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{e}")})),
        )
            .into_response(),
    }
}

/// Force an OpenRouter pricing sync. Returns the row count.
pub async fn sync_pricing_now(State(state): State<AppState>) -> Response {
    let cfg = state.config.snapshot().await.pricing_sync;
    match crate::cost::sync_once(&state.pipeline.http, &state.db, &cfg.openrouter_url).await {
        Ok(n) => (
            StatusCode::OK,
            Json(json!({"status": "ok", "upserted": n, "url": cfg.openrouter_url})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": format!("{e}")})),
        )
            .into_response(),
    }
}
