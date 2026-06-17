//! OpenAI-compatible image / audio / video endpoints. Pass-through
//! proxy: extract `model` from the body, resolve to a provider via
//! the registry, forward the body verbatim, return the response.
//!
//! No body reshaping (per design doc). Different shape from chat —
//! caller is responsible for the OpenAI request envelope.

use super::AppState;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

use crate::auth::resolve as resolve_key_async;

/// Shared handler for image / audio / video. `path_suffix` is the
/// OpenAI-style endpoint, e.g. `/v1/images/generations`.
pub async fn passthrough(
    State(state): State<AppState>,
    path_suffix: &'static str,
    Json(body): Json<Value>,
) -> Response {
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if model.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"message": "model field required", "type": "invalid_request_error"}})),
        )
            .into_response();
    }

    // Resolve provider. Accept provider/model or tier/provider/model.
    let (provider_id, _model_id) = match super::super::providers::ProviderRegistry::split_model_ref(&model) {
        Some(pm) => pm,
        None => {
            // Try tier/provider/model
            let parts: Vec<&str> = model.splitn(3, '/').collect();
            if parts.len() == 3 {
                (parts[1].to_string(), parts[2].to_string())
            } else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": {"message": format!("invalid model ref: {model}"), "type": "invalid_request_error"}})),
                )
                    .into_response();
            }
        }
    };

    let adapter = match state.pipeline.registry.get(&provider_id).await {
        Some(a) => a,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": {"message": format!("unknown provider: {provider_id}"), "type": "invalid_request_error"}})),
            )
                .into_response();
        }
    };

    let snap = state.config.snapshot().await;
    let cfg_key = snap
        .providers
        .iter()
        .find(|p| p.id == provider_id)
        .and_then(|p| p.key.as_deref());
    let key = resolve_key_async(&state.key_store, &provider_id, cfg_key).await;
    if key.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"message": format!("no API key for {provider_id}"), "type": "invalid_request_error"}})),
        )
            .into_response();
    }

    // Build upstream URL: base_url + path_suffix
    let base_url = adapter.base_url().trim_end_matches('/');
    let url = format!("{base_url}{path_suffix}");

    // Build auth header
    let (auth_name, auth_val) = adapter.auth_header(&key);
    let mut req_headers = reqwest::header::HeaderMap::new();
    req_headers.insert(auth_name, auth_val);
    req_headers.insert("content-type", "application/json".parse().unwrap());

    // Forward the body verbatim
    let body_for_post = body.clone();
    let resp = match state
        .pipeline
        .http
        .post(&url)
        .headers(req_headers)
        .json(&body_for_post)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, format!("upstream send: {e}")),
    };

    let status = StatusCode::from_u16(resp.status().as_u16())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let body = resp.bytes().await.unwrap_or_default();

    // Log it (fire-and-forget)
    let provider_for_log = provider_id.clone();
    let path_for_log = path_suffix.to_string();
    let model_for_log = model.clone();
    let db = state.db.clone();
    tokio::spawn(async move {
        let _ = db
            .with(move |conn| {
                let request_id = uuid::Uuid::new_v4().to_string();
                crate::db::queries::insert_request(
                    conn,
                    &crate::db::queries::RequestLog {
                        id: request_id,
                        tier: "multimodal".to_string(),
                        requested_model: Some(model_for_log),
                        routed_model: "passthrough".to_string(),
                        routed_provider: provider_for_log,
                        total_latency_ms: 0,
                        input_tokens: None,
                        output_tokens: None,
                        cache_read_tokens: None,
                        cost_usd: None,
                        truncated: false,
                        fallback_count: 0,
                        finished: true,
                        finish_reason: Some(if status.is_success() { "ok" } else { "error" }.to_string()),
                        client_ip: None,
                        user_id: None,
                        user_agent: None,
                    },
                )
                .map_err(|e| anyhow::anyhow!("log insert: {e}"))
            })
            .await;
        let _ = path_for_log;
    });

    let mut response_headers = axum::http::HeaderMap::new();
    if let Ok(v) = axum::http::HeaderValue::from_str(&ct) {
        response_headers.insert("content-type", v);
    }
    if let Ok(v) = axum::http::HeaderValue::from_str(&provider_id) {
        response_headers.insert("x-router-provider", v);
    }
    if let Ok(v) = axum::http::HeaderValue::from_str(&model) {
        response_headers.insert("x-router-model", v);
    }
    (status, response_headers, body).into_response()
}

fn error_response(status: StatusCode, message: String) -> Response {
    (
        status,
        Json(json!({"error": {"message": message, "type": "server_error"}})),
    )
        .into_response()
}

pub async fn image_generations(
    state: State<AppState>,
    body: Json<Value>,
) -> Response {
    passthrough(state, "/v1/images/generations", body).await
}

pub async fn audio_speech(
    state: State<AppState>,
    body: Json<Value>,
) -> Response {
    passthrough(state, "/v1/audio/speech", body).await
}

pub async fn video_generations(
    state: State<AppState>,
    body: Json<Value>,
) -> Response {
    passthrough(state, "/v1/videos/generations", body).await
}
