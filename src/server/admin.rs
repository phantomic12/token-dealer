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

use crate::config::types::{DetectionCondition, DetectionRule, ProviderConfig, ProviderType, TierConfig};

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
            Ok(_) => (StatusCode::CREATED, Json(json!({"status": "added", "id": body.id})))
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

pub async fn remove_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
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
        Ok(_) => (StatusCode::OK, Json(json!({"status": "updated", "tier": tier}))).into_response(),
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
        Ok(_) => (StatusCode::OK, Json(json!({"status": "stored", "provider": provider_id}))).into_response(),
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
        Ok(_) => (StatusCode::OK, Json(json!({"status": "deleted", "provider": provider_id}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("delete failed: {e}")})),
        )
            .into_response(),
    }
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

pub async fn add_rule(
    State(state): State<AppState>,
    Json(body): Json<AddRuleRequest>,
) -> Response {
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
        Ok(_) => (StatusCode::OK, Json(json!({"status": "deleted", "index": index}))).into_response(),
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
        return html_test_result(
            is_htmx,
            false,
            "id required".to_string(),
        );
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
        ProviderType::Anthropic => state
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
            .await,
        ProviderType::Google => state
            .pipeline
            .http
            .get(format!("{base_url}/v1beta/models"))
            .header("x-goog-api-key", &key)
            .send()
            .await,
        ProviderType::Kiro => state
            .pipeline
            .http
            .get(format!("{base_url}/ping"))
            .header("authorization", format!("Bearer {key}"))
            .send()
            .await,
        _ => state
            .pipeline
            .http
            .get(format!("{base_url}/v1/models"))
            .header("authorization", format!("Bearer {key}"))
            .send()
            .await,
    };

    let resp = match req_result {
        Ok(r) => r,
        Err(e) => {
            return html_test_result(
                is_htmx,
                false,
                format!("connection failed: {e}"),
            );
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
        (StatusCode::OK, Json(json!({"status": "ok", "message": message}))).into_response()
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
