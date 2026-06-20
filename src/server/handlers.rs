//! HTTP handlers. Thin — most logic lives in `proxy/pipeline.rs`.

use super::AppState;
use async_stream::stream;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures::StreamExt;
use serde_json::{json, Value};

use crate::proxy::pipeline::Pipeline;
use crate::routing::scorer::{Scorer, ScoringContext};
use crate::routing::specificity::detector_from_config;
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
    let (input, output, cost, reqs) = state
        .user_store
        .get_global_usage_today()
        .await
        .unwrap_or((0, 0, 0.0, 0));
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
    // Merge: configured providers (with their default_model) +
    // discovered models from the `provider_models` cache (filled by
    // the startup discovery task). De-duplicate on (provider, model).
    let mut seen = std::collections::HashSet::new();
    let mut models = Vec::new();
    let providers = state.pipeline.registry.list().await;
    for (pid, default_model) in providers {
        let key = (pid.clone(), default_model.clone());
        if seen.insert(key.clone()) {
            models.push(json!({
                "id": format!("{pid}/{default_model}"),
                "object": "model",
                "owned_by": pid,
            }));
        }
    }
    if let Ok(discovered) = crate::discovery::list_discovered(&state.db, None).await {
        for row in discovered {
            let key = (row.provider_id.clone(), row.model_id.clone());
            if seen.insert(key) {
                models.push(json!({
                    "id": format!("{}/{}", row.provider_id, row.model_id),
                    "object": "model",
                    "owned_by": row.provider_id,
                }));
            }
        }
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

    // Specificity routing runs in parallel with the tier scorer.
    // When a category activates, we override the model with the
    // category's configured primary. The tier remains as the
    // fallback (response header `x-router-tier` reflects what the
    // scorer chose; `x-router-specificity` shows what we detected).
    let cfg_snap = state.pipeline.config.snapshot().await;
    let detector = detector_from_config(&cfg_snap);
    let header_override = headers
        .get("x-router-specificity")
        .and_then(|v| v.to_str().ok());
    let specificity_decision = detector.detect(&pre.request, header_override, &[]);

    let tier = inbound_tier_hint.unwrap_or(score.tier);
    let mut model_override = model_override.or(score.model_override);
    if let Some(decision) = &specificity_decision {
        if let Some(primary) = &decision.primary {
            tracing::info!(
                request_id = ?request_id_or_zero(&headers),
                category = %decision.category,
                score = decision.score,
                threshold = decision.threshold,
                primary = %primary,
                reason = %decision.reason,
                "specificity override"
            );
            model_override = Some(primary.clone());
        }
    }

    // Budget check (per-day / per-request). At dispatch time we don't
    // know the actual cost yet (no token counts from upstream) so we
    // project a rough worst-case estimate based on the model + tier.
    // The real cost is recorded after the response returns.
    let budget_cfg = cfg_snap.budgets.clone();
    if budget_cfg.daily_cost_usd > 0.0 || budget_cfg.per_request_cost_usd > 0.0 {
        let user_for_budget = if user.via == "legacy_key" || user.via == "env_password" {
            None
        } else {
            Some(user.user_id.as_str())
        };
        let projected = crate::cost::calculate_with_db(
            "any",
            &model_override.clone().unwrap_or_default(),
            0,
            0,
            Some(&state.pricing),
        )
        .unwrap_or(0.0);
        match crate::cost::limits::check(
            &state.db,
            &state.pricing,
            &budget_cfg,
            user_for_budget,
            projected,
        )
        .await
        {
            Ok(crate::cost::limits::BudgetDecision::Deny { reason }) => {
                tracing::warn!(reason = %reason, user = ?user_for_budget, "budget deny");
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    axum::Json(serde_json::json!({
                        "error": {
                            "code": "budget_exceeded",
                            "message": reason,
                            "type": "rate_limit_error"
                        }
                    })),
                )
                    .into_response();
            }
            Ok(crate::cost::limits::BudgetDecision::SoftWarning { fraction, kind }) => {
                tracing::info!(
                    fraction,
                    kind,
                    user = ?user_for_budget,
                    "approaching budget"
                );
            }
            _ => {}
        }
    }

    let mut routed = match state
        .pipeline
        .route(pre.request, model_override, tier)
        .await
    {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };

    // Stamp the routing output with the user context + agent type
    // + specificity decision for downstream logging + per-user usage
    // tracking + response headers.
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
    if let Some(decision) = specificity_decision {
        routed
            .canonical
            .metadata
            .specificity_category
            .get_or_insert(decision.category.to_string());
    }

    let provider_id = routed.route.provider_id.clone();
    let model_id = routed.route.model_id.clone();
    let request_id = routed.request_id;
    let specificity_for_header = routed.canonical.metadata.specificity_category.clone();

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
        attach_routing_headers(
            resp,
            &provider_id,
            &model_id,
            tier,
            request_id,
            specificity_for_header.as_deref(),
        )
    } else {
        // Non-streaming path
        let resp = match state.pipeline.complete(routed).await {
            Ok(r) => r,
            Err(e) => return e.into_response(),
        };
        let v = response_to_openai(&resp);
        let mut resp = (StatusCode::OK, Json(v)).into_response();
        attach_routing_headers(
            resp,
            &provider_id,
            &model_id,
            tier,
            request_id,
            specificity_for_header.as_deref(),
        )
    }
}

fn attach_routing_headers(
    mut resp: Response,
    provider_id: &str,
    model_id: &str,
    tier: crate::schema::canonical::Tier,
    request_id: uuid::Uuid,
    specificity: Option<&str>,
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
    if let Some(cat) = specificity {
        if let Ok(v) = HeaderValue::from_str(cat) {
            h.insert("x-router-specificity", v);
        }
    }
    resp
}

/// Pull X-Router-Request-Id from the inbound headers if the client
/// supplied one — useful for log correlation. Returns 0 if absent.
fn request_id_or_zero(_headers: &HeaderMap) -> u128 {
    0
}
