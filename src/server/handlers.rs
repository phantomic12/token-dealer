//! HTTP handlers. Thin — most logic lives in `proxy/pipeline.rs`.

use super::AppState;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use async_stream::stream;
use futures::StreamExt;
use serde_json::{json, Value};

use crate::proxy::pipeline::Pipeline;
use crate::routing::scorer::{Scorer, ScoringContext};
use crate::schema::inbound::parse_inbound;
use crate::schema::outbound::{chunk_to_openai, done_sentinel, response_to_openai};

pub async fn health() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"status": "ok"})))
}

/// Public stats for the marketing site / landing page. No auth
/// required. Returns aggregate counters — total requests served,
/// total tokens, total USD cost, top providers. Updated continuously
/// from the token_usage + request_log tables.
pub async fn public_stats(State(state): State<AppState>) -> Response {
    let (input, output, cost, reqs) = state.user_store.get_global_usage_today().await.unwrap_or((0, 0, 0.0, 0));
    let snap = state.config.snapshot().await;
    let provider_count = snap.providers.len();
    Json(json!({
        "today": {
            "input_tokens": input,
            "output_tokens": output,
            "cost_usd": cost,
            "request_count": reqs,
        },
        "providers_configured": provider_count,
        "tiers": snap.tiers.len(),
    }))
    .into_response()
}

/// Liveness/readiness — no auth, no DB.
pub async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"status": "ok"})))
}

pub async fn list_models(State(state): State<AppState>) -> impl IntoResponse {
    let providers = state.pipeline.registry.list().await;
    let mut models = Vec::new();
    for (pid, default_model) in providers {
        models.push(json!({
            "id": format!("{pid}/{default_model}"),
            "object": "model",
            "owned_by": pid,
        }));
    }
    Json(json!({"object": "list", "data": models}))
}

pub async fn reload_config(State(state): State<AppState>) -> impl IntoResponse {
    match state.pipeline.config.reload().await {
        Ok(_) => (StatusCode::OK, Json(json!({"status": "reloaded"}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Extension(user): axum::extract::Extension<crate::auth::UserContext>,
    Json(body): Json<Value>,
) -> Response {
    let pre = match parse_inbound(body) {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };

    // Extract explicit `tier/provider/model` form so we can pass a
    // model_override into routing. Otherwise the scorer decides tier.
    let mut model_override: Option<String> = None;
    let inbound_tier_hint: Option<crate::schema::canonical::Tier> = {
        // tier/provider/model — three segments after the leading tier
        let parts: Vec<&str> = pre.model_string.splitn(3, '/').collect();
        if parts.len() == 3 {
            if let Some(t) = crate::schema::canonical::Tier::parse(parts[0]) {
                model_override = Some(pre.model_string.clone());
                Some(t)
            } else {
                None
            }
        } else {
            None
        }
    };

    let scorer = Scorer::new(state.pipeline.config.clone());
    let score = scorer
        .score(ScoringContext {
            inbound: &pre.request,
            headers: &headers,
        })
        .await;
    let tier = inbound_tier_hint.unwrap_or(score.tier);
    let model_override = model_override.or(score.model_override);
    let mut routed = match state
        .pipeline
        .route(pre.request, model_override, tier)
        .await
    {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };

    // Stamp the routing output with the user context + agent type
    // for downstream logging + per-user usage tracking. The user
    // is "anonymous" when auth is disabled (legacy single-tenant mode).
    routed.user_id = if user.via == "legacy_key" || user.via == "env_password" {
        None
    } else {
        Some(user.user_id.clone())
    };
    if let Some(ua) = headers.get("user-agent").and_then(|v| v.to_str().ok()) {
        routed.user_agent = Some(ua.to_string());
        routed.canonical.metadata.agent_type =
            Some(crate::agents::detect_agent(Some(ua)).as_str().to_string());
    }

    let provider_id = routed.route.provider_id.clone();
    let model_id = routed.route.model_id.clone();
    let request_id = routed.request_id;

    // Per-request key override: `X-Router-Key: <key>` bypasses the
    // resolved key. Used to swap in a different upstream key without
    // touching the config (e.g. tenant-specific billing, ad-hoc
    // testing). The inbound Authorization header is still validated
    // by the auth middleware — this only swaps the UPSTREAM key.
    let mut routed = routed;
    if let Some(override_key) = headers.get("x-router-key").and_then(|v| v.to_str().ok()) {
        if !override_key.is_empty() {
            tracing::info!(
                request_id = %request_id,
                provider = %provider_id,
                "using X-Router-Key override for upstream"
            );
            routed.key = override_key.to_string();
        }
    }

    if routed.canonical.stream {
        // Streaming path
        let stream_res = state.pipeline.stream(routed).await;
        let s = match stream_res {
            Ok(s) => s,
            Err(e) => return e.into_response(),
        };
        // Wrap the provider stream and convert CanonicalChunk → OpenAI chunk
        let model_id_for_stream = model_id.clone();
        let id_for_stream = request_id.to_string();
        let pid_for_stream = provider_id.clone();
        let id_for_done = id_for_stream.clone();
        let chunk_stream = futures::stream::unfold(s.boxed(), move |mut s| {
            let model_id = model_id_for_stream.clone();
            let id = id_for_stream.clone();
            let pid = pid_for_stream.clone();
            async move {
                match s.next().await {
                    Some(Ok(chunk)) => {
                        let _ = (model_id, id, pid);
                        Some((Ok::<_, crate::error::AppError>(chunk), s))
                    }
                    Some(Err(e)) => Some((Err(e), s)),
                    None => None,
                }
            }
        });

        use axum::response::sse::{Event, KeepAlive, Sse};
        let model_id_for_body = model_id.clone();
        let request_budget_ms = state
            .pipeline
            .config
            .snapshot()
            .await
            .retry
            .request_budget_ms;
        let started = std::time::Instant::now();
        let body = stream! {
            tokio::pin!(chunk_stream);
            while let Some(item) = chunk_stream.next().await {
                // Mid-stream budget enforcement. Emits a terminal
                // error chunk and closes the stream cleanly so the
                // client gets a proper SSE terminator.
                if started.elapsed().as_millis() as u64 > request_budget_ms {
                    tracing::warn!(
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        budget_ms = request_budget_ms,
                        "stream exceeded request budget; closing"
                    );
                    yield Ok(Event::default()
                        .event("error")
                        .data(json!({"error": "request budget exceeded"}).to_string()));
                    break;
                }
                match item {
                    Ok(chunk) => {
                        let mut c = chunk;
                        if c.model.is_empty() {
                            c.model = model_id_for_body.clone();
                        }
                        if c.id.is_empty() {
                            c.id = id_for_done.clone();
                        }
                        if let Some(v) = chunk_to_openai(&c) {
                            yield Ok::<_, std::convert::Infallible>(
                                Event::default().data(v.to_string()),
                            );
                        }
                    }
                    Err(e) => {
                        yield Ok(Event::default()
                            .event("error")
                            .data(json!({"error": e.to_string()}).to_string()));
                        break;
                    }
                }
            }
            yield Ok(Event::default().data(done_sentinel().to_string()));
        };
        let sse = Sse::new(body).keep_alive(KeepAlive::new());
        let resp = sse.into_response();
        attach_routing_headers(resp, &provider_id, &model_id, tier, request_id)
    } else {
        // Non-streaming path
        let resp = match state.pipeline.complete(routed).await {
            Ok(r) => r,
            Err(e) => return e.into_response(),
        };
        let v = response_to_openai(&resp);
        let mut resp = (StatusCode::OK, Json(v)).into_response();
        attach_routing_headers(resp, &provider_id, &model_id, tier, request_id)
    }
}

fn attach_routing_headers(
    mut resp: Response,
    provider_id: &str,
    model_id: &str,
    tier: crate::schema::canonical::Tier,
    request_id: uuid::Uuid,
) -> Response {
    use axum::http::HeaderValue;
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(model_id) {
        h.insert("x-router-model", v);
    }
    if let Ok(v) = HeaderValue::from_str(provider_id) {
        h.insert("x-router-provider", v);
    }
    if let Ok(v) = HeaderValue::from_str(tier.as_str()) {
        h.insert("x-router-tier", v);
    }
    if let Ok(v) = HeaderValue::from_str(&request_id.to_string()) {
        h.insert("x-router-request-id", v);
    }
    resp
}
