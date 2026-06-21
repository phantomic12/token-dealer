//! v0.2.0 plan item 4: pass-through handlers for `/v1/messages`
//! (Anthropic Messages API) and `/v1/responses` (OpenAI Responses
//! API). No cross-shape transpilation in v0.2.0; the inbound
//! path determines the expected wire format, and a tier primary
//! whose adapter type doesn't speak that format is rejected with
//! a 400 that names both the expected and the actual format.
//!
//! The plan's exact rejection text is reproduced verbatim so
//! downstream tooling can pattern-match it: `"/v1/messages
//! received but tier primary is <id>/<format>; use
//! /v1/chat/completions or change tier primary"`.
//!
//! Streaming: SSE chunks pass through unchanged on matching
//! shapes. The body is forwarded with `stream: true` honored
//! (Accept: text/event-stream, no buffering).
//!
//! Transpilation (Anthropic↔OpenAI, Responses↔ChatCompletions)
//! is deferred to v0.3+.

use axum::http::HeaderMap;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

use super::AppState;

/// Wire format required for `/v1/messages` (Anthropic Messages
/// shape). Per the plan, this is Anthropic and Kiro.
fn is_messages_compatible(adapter_kind: &str) -> bool {
    matches!(adapter_kind, "anthropic" | "kiro")
}

/// Wire format required for `/v1/responses` (OpenAI Responses
/// shape). Per the plan, this is OpenAI and OpenRouter (and the
/// dedicated `responses` adapter type, which is a stand-in for
/// OpenAI's new Responses API).
fn is_responses_compatible(adapter_kind: &str) -> bool {
    matches!(adapter_kind, "openai" | "openrouter" | "responses")
}

/// Resolve the wire-format name from a provider's adapter type.
/// Used to make the rejection message explicit. All the
/// OpenAI-compatible aliases (Groq, Deepseek, Fireworks, etc.)
/// collapse to "openai" — they all speak the Chat Completions
/// wire format and the Responses API when the upstream supports
/// it.
fn adapter_wire_format(provider_type: &crate::config::ProviderType) -> &'static str {
    use crate::config::ProviderType::*;
    match provider_type {
        Anthropic => "anthropic",
        Kiro => "kiro",
        Openai => "openai",
        Openrouter => "openrouter",
        Responses => "responses",
        Google => "google",
        Generic => "generic",
        Tokenrouter | Groq | Deepseek | Fireworks | Mistral | Xai | Qwen | Moonshot | Zai
        | Xiaomi | Minimax | Byteplus | Nvidia | OpencodeGo | OpencodeZen | Kilo | Commandcode
        | GithubCopilot | Gitlawb | Ollama | OllamaCloud | LlamaCpp | LmStudio => "openai",
    }
}

/// Shared pass-through handler. `endpoint` is the upstream path
/// (e.g. `/v1/messages`); `expected_format` is the wire-format
/// name we want; `check_fn` decides whether the resolved
/// adapter is compatible.
async fn passthrough_wire(
    state: &AppState,
    body: Value,
    endpoint: &'static str,
    expected_format: &'static str,
    check_fn: fn(&str) -> bool,
) -> Response {
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if model.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": {
                    "message": "model field required",
                    "type": "invalid_request_error",
                }
            })),
        )
            .into_response();
    }
    let (provider_id, _model_id) = match crate::providers::ProviderRegistry::split_model_ref(&model)
    {
        Some(pm) => pm,
        None => {
            let parts: Vec<&str> = model.splitn(3, '/').collect();
            if parts.len() == 3 {
                (parts[1].to_string(), parts[2].to_string())
            } else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": {
                            "message": format!("invalid model ref: {model}"),
                            "type": "invalid_request_error",
                        }
                    })),
                )
                    .into_response();
            }
        }
    };
    let snap = state.config.snapshot().await;
    let provider_cfg = snap.providers.iter().find(|p| p.id == provider_id);
    let adapter_kind = provider_cfg
        .map(|p| adapter_wire_format(&p.provider_type))
        .unwrap_or("unknown");
    if !check_fn(adapter_kind) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": {
                    "message": format!(
                        "/{endpoint} received but tier primary is {provider_id}/{adapter_kind}; \
                         use /v1/chat/completions or change tier primary"
                    ),
                    "type": "invalid_request_error",
                    "code": "wire_format_mismatch",
                    "expected_format": expected_format,
                    "actual_format": adapter_kind,
                }
            })),
        )
            .into_response();
    }
    let adapter = match state.pipeline.registry.get(&provider_id).await {
        Some(a) => a,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": {
                        "message": format!("unknown provider: {provider_id}"),
                        "type": "invalid_request_error",
                    }
                })),
            )
                .into_response();
        }
    };
    let cfg_key = provider_cfg.and_then(|p| p.key.as_deref());
    let key = crate::auth::resolve(&state.key_store, &state.master, &provider_id, cfg_key).await;
    if key.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": {
                    "message": format!("no API key for {provider_id}"),
                    "type": "invalid_request_error",
                }
            })),
        )
            .into_response();
    }
    let base_url = adapter.base_url().trim_end_matches('/');
    let url = format!("{base_url}{endpoint}");
    let (auth_name, auth_val) = adapter.auth_header(&key);
    let mut req_headers = reqwest::header::HeaderMap::new();
    req_headers.insert(auth_name, auth_val);
    req_headers.insert("content-type", "application/json".parse().unwrap());
    let stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if stream {
        req_headers.insert("accept", "text/event-stream".parse().unwrap());
    }
    let upstream = state
        .pipeline
        .http
        .post(&url)
        .headers(req_headers)
        .json(&body)
        .send()
        .await;
    let resp = match upstream {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": {
                        "message": format!("upstream error: {e}"),
                        "type": "upstream_error",
                    }
                })),
            )
                .into_response();
        }
    };
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut out_headers = HeaderMap::new();
    if let Some(ct) = resp.headers().get("content-type") {
        if let Ok(v) = ct.to_str() {
            if let Ok(hv) = v.parse() {
                out_headers.insert("content-type", hv);
            }
        }
    }
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": {
                        "message": format!("upstream read error: {e}"),
                        "type": "upstream_error",
                    }
                })),
            )
                .into_response();
        }
    };
    (status, out_headers, bytes).into_response()
}

/// POST /v1/messages — Anthropic Messages API pass-through.
/// Tier primary must be Anthropic or Kiro; otherwise 400.
pub async fn messages_passthrough(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    passthrough_wire(
        &state,
        body,
        "/v1/messages",
        "anthropic",
        is_messages_compatible,
    )
    .await
}

/// POST /v1/responses — OpenAI Responses API pass-through.
/// Tier primary must be OpenAI / OpenRouter; otherwise 400.
pub async fn responses_passthrough(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    passthrough_wire(
        &state,
        body,
        "/v1/responses",
        "openai-responses",
        is_responses_compatible,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_compatible_against_known_kinds() {
        assert!(is_messages_compatible("anthropic"));
        assert!(is_messages_compatible("kiro"));
        assert!(!is_messages_compatible("openai"));
        assert!(!is_messages_compatible("google"));
    }

    #[test]
    fn responses_compatible_against_known_kinds() {
        assert!(is_responses_compatible("openai"));
        assert!(is_responses_compatible("openrouter"));
        assert!(!is_responses_compatible("anthropic"));
        assert!(!is_responses_compatible("kiro"));
    }

    #[test]
    fn adapter_wire_format_maps_known_types() {
        use crate::config::ProviderType::*;
        assert_eq!(adapter_wire_format(&Anthropic), "anthropic");
        assert_eq!(adapter_wire_format(&Kiro), "kiro");
        assert_eq!(adapter_wire_format(&Openai), "openai");
        assert_eq!(adapter_wire_format(&Openrouter), "openrouter");
        // OpenAI-compatible adapters reuse the openai wire format.
        assert_eq!(adapter_wire_format(&Groq), "openai");
        assert_eq!(adapter_wire_format(&Deepseek), "openai");
    }
}
