//! The request pipeline: tier → provider → adapter → response.
//! Non-streaming path: executes the full fallback chain (primary +
//! fallbacks) with one retry per provider on 429/408. Streaming
//! path: commits to the primary; retries are still possible
//! before any bytes hit the client, but mid-stream fallbacks are
//! not (per design).

use super::super::auth::resolve as resolve_key;
use super::super::config::ConfigService;
use super::super::error::{AppError, AppResult};
use super::super::providers::ProviderRegistry;
use super::super::schema::canonical::{CanonicalRequest, CanonicalResponse, Tier};
use super::super::schema::inbound::{InboundRequest, PreRouting};
use super::fallback::{self, ExecutionResult, ProviderHandle, RoutingPlan};
use crate::routing::selector::{SelectedRoute, Selector};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

#[derive(Clone)]
pub struct Pipeline {
    pub registry: Arc<ProviderRegistry>,
    pub config: ConfigService,
    pub selector: Selector,
    pub http: reqwest::Client,
    pub db: crate::db::Db,
    pub health: crate::providers::HealthRegistry,
    pub key_store: crate::auth::KeyStore,
    /// Master key for decrypting `enc:`-prefixed values at
    /// dispatch time. Mirrors the AppState field.
    pub master: crate::auth::MasterKey,
    pub oauth: crate::oauth::OAuthManager,
    pub user_store: crate::auth::UserStore,
    pub pricing: crate::cost::PricingStore,
}

pub struct RoutingOutput {
    pub canonical: CanonicalRequest,
    pub route: SelectedRoute,
    pub key: String,
    pub request_id: Uuid,
    /// Per-request user context (set by the chat handler before
    /// dispatching to the pipeline). Optional — anonymous requests
    /// leave it as `None`.
    pub user_id: Option<String>,
    pub user_agent: Option<String>,
}

impl Pipeline {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        registry: Arc<ProviderRegistry>,
        config: ConfigService,
        http: reqwest::Client,
        db: crate::db::Db,
        health: crate::providers::HealthRegistry,
        key_store: crate::auth::KeyStore,
        master: crate::auth::MasterKey,
        oauth: crate::oauth::OAuthManager,
        user_store: crate::auth::UserStore,
        pricing: crate::cost::PricingStore,
    ) -> Self {
        let selector = Selector::new(registry.clone());
        Self {
            registry,
            config,
            selector,
            http,
            db,
            health,
            key_store,
            master,
            oauth,
            user_store,
            pricing,
        }
    }

    /// Resolve the inbound request into a routing decision. The
    /// caller then runs the chosen adapter.
    pub async fn route(
        &self,
        inbound: InboundRequest,
        model_override: Option<String>,
        tier: Tier,
    ) -> AppResult<RoutingOutput> {
        let cfg = self.config.snapshot().await;

        let route = if let Some(m) = model_override {
            self.selector.route_explicit(&m).await.ok_or_else(|| {
                AppError::BadRequest(format!("unknown provider in model ref: {m}"))
            })?
        } else {
            self.selector.route_tier(&cfg, tier).await.ok_or_else(|| {
                AppError::Internal(format!("no primary configured for tier {}", tier.as_str()))
            })?
        };

        let request_id = Uuid::new_v4();
        let canonical = inbound.into_canonical(
            tier,
            route.model_id.clone(),
            route.provider_id.clone(),
            request_id,
        )?;

        let key = {
            let tier_override = cfg.tier_key_override(tier);
            let cfg_key = cfg
                .providers
                .iter()
                .find(|p| p.id == route.provider_id)
                .and_then(|p| p.key.as_deref())
                .or(tier_override);
            // If the provider has OAuth config, prefer the access token
            // from the OAuth manager. The user-facing `key` field is
            // treated as a refresh token in that case.
            let resolved =
                resolve_key(&self.key_store, &self.master, &route.provider_id, cfg_key).await;
            if let Some(pt) = crate::providers::resolve_alias(&route.provider_id) {
                if let Some(m) = crate::providers::manifest::lookup(pt) {
                    if m.oauth.is_some() {
                        if let Ok(Some(access)) = self.oauth.access_token(&route.provider_id).await
                        {
                            access
                        } else {
                            // OAuth not set up yet — fall back to the
                            // raw refresh token (some endpoints accept
                            // it directly; otherwise the next call
                            // will surface a 401 and the user knows
                            // to set the refresh token via the UI).
                            resolved
                        }
                    } else {
                        resolved
                    }
                } else {
                    resolved
                }
            } else {
                resolved
            }
        };
        if key.is_empty() {
            return Err(AppError::Internal(format!(
                "no API key for provider {}",
                route.provider_id
            )));
        }

        Ok(RoutingOutput {
            canonical,
            route,
            key,
            request_id,
            user_id: None,
            user_agent: None,
        })
    }

    /// Non-streaming execution with full fallback chain. The canonical
    /// request in `routed.canonical` is treated as the request to send
    /// to the primary; the fallback executor walks the chain.
    pub async fn complete(&self, routed: RoutingOutput) -> AppResult<CanonicalResponse> {
        let cfg = self.config.snapshot().await;
        let tier_cfg = cfg.tiers.get(routed.canonical.tier.as_str()).cloned();
        let primary = format!("{}/{}", routed.route.provider_id, routed.route.model_id);
        let fallbacks = tier_cfg
            .as_ref()
            .map(|t| t.fallbacks.clone())
            .unwrap_or_default();
        let downgrade_to = tier_cfg.and_then(|t| t.downgrade_to);

        let plan = RoutingPlan {
            request: routed.canonical.clone(),
            primary,
            fallbacks,
            downgrade_to,
            request_budget: Duration::from_millis(cfg.retry.request_budget_ms),
            max_retries_per_provider: cfg.retry.max_same_provider_retries,
            max_retry_after_ms: cfg.retry.max_retry_after_ms,
            fixed_retry_wait_ms: cfg.retry.fixed_retry_wait_ms,
        };

        let registry = self.registry.clone();
        let config = self.config.clone();
        let key_store = self.key_store.clone();
        let master = self.master.clone();
        let health_hook = super::fallback::HealthHook {
            registry: self.health.clone(),
            failure_threshold: cfg.retry.max_same_provider_retries.max(1),
            cooldown_secs: 60, // default; tunable via config in phase 2
        };
        let result = fallback::execute(
            plan,
            move |pid: &str| {
                let r = registry.clone();
                let c = config.clone();
                let ks = key_store.clone();
                let m = master.clone();
                let p = pid.to_string();
                async move {
                    let g = r.read().await;
                    let adapter = g.get(&p)?.clone();
                    let snap = c.snapshot().await;
                    let cfg_key = snap
                        .providers
                        .iter()
                        .find(|prov| prov.id == p)
                        .and_then(|prov| prov.key.as_deref());
                    let key = resolve_key(&ks, &m, &p, cfg_key).await;
                    Some(ProviderHandle {
                        provider_id: p,
                        model_id: adapter.default_model().to_string(),
                        adapter,
                        key,
                    })
                }
            },
            &health_hook,
        )
        .await?;

        self.log_completion(&routed, &result).await;
        Ok(result.response)
    }

    /// Streaming execution. For MVP: commits to the primary; falls
    /// back only on a transport-level error (before any chunk has
    /// been sent). Mid-stream failure emits a terminal error chunk.
    pub async fn stream(
        &self,
        routed: RoutingOutput,
    ) -> AppResult<super::super::providers::ProviderStream> {
        let adapter = self
            .registry
            .get(&routed.route.provider_id)
            .await
            .ok_or_else(|| {
                AppError::Internal(format!(
                    "provider disappeared: {}",
                    routed.route.provider_id
                ))
            })?;
        adapter
            .stream(&routed.canonical, &routed.key, &self.http)
            .await
    }

    async fn log_completion(&self, routed: &RoutingOutput, result: &ExecutionResult) {
        use crate::db::queries::{AttemptLog, RequestLog};
        let input_tokens = result.response.usage.input_tokens;
        let output_tokens = result.response.usage.output_tokens;
        let cost = crate::cost::calculate_with_db(
            &result.response.provider,
            &result.response.model,
            input_tokens,
            output_tokens,
            Some(&self.pricing),
        );
        let log = RequestLog {
            id: routed.request_id.to_string(),
            tier: routed.canonical.tier.as_str().to_string(),
            requested_model: Some(routed.canonical.selected_model.clone()),
            routed_model: result.response.model.clone(),
            routed_provider: result.response.provider.clone(),
            total_latency_ms: result.attempts.last().map(|a| a.latency_ms).unwrap_or(0),
            input_tokens: Some(input_tokens),
            output_tokens: Some(output_tokens),
            cache_read_tokens: result.response.usage.cache_read_tokens,
            cost_usd: cost,
            truncated: false,
            fallback_count: result.fallback_count,
            finished: true,
            finish_reason: result.response.finish_reason.clone(),
            client_ip: None,
            user_id: routed.user_id.clone(),
            user_agent: routed.user_agent.clone(),
        };
        crate::log::log_request(&self.db, log);
        // Per-user daily token usage + cost. Bumps the row keyed by
        // (user_id, day) atomically.
        if let (Some(uid), Some(c)) = (routed.user_id.as_ref(), cost) {
            let _ = self
                .user_store
                .record_usage(uid, input_tokens, output_tokens, c)
                .await;
        }
        for (idx, a) in result.attempts.iter().enumerate() {
            crate::log::log_attempt(
                &self.db,
                AttemptLog {
                    request_id: routed.request_id.to_string(),
                    attempt_number: (idx + 1) as u32,
                    provider: a.provider.clone(),
                    model: a.model.clone(),
                    outcome: format!("{:?}", a.outcome),
                    error_code: None,
                    error_message: None,
                    latency_ms: a.latency_ms,
                    retry_wait_ms: None,
                },
            );
        }
    }
}

/// Helper for the handler: extract a (PreRouting, optional override)
/// from a raw JSON body + headers, given a scorer.
pub async fn resolve_route(
    pipeline: &Pipeline,
    pre: PreRouting,
    tier: Tier,
    model_override: Option<String>,
) -> AppResult<RoutingOutput> {
    pipeline.route(pre.request, model_override, tier).await
}
