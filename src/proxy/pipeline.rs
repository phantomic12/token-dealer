//! The request pipeline: tier → provider → adapter → response.
//! MVP: single attempt, no fallbacks. The "all providers failed"
//! path returns the last error to the client. Fallback chains and
//! circuit-breaker probes are wired in `providers/health.rs` and
//! activated in phase 2.

use super::super::config::ConfigService;
use super::super::error::{AppError, AppResult};
use super::super::providers::registry::resolve_key;
use super::super::providers::ProviderRegistry;
use super::super::schema::canonical::{CanonicalRequest, CanonicalResponse, Tier};
use super::super::schema::inbound::{InboundRequest, PreRouting};
use crate::routing::selector::{SelectedRoute, Selector};
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

#[derive(Clone)]
pub struct Pipeline {
    pub registry: Arc<ProviderRegistry>,
    pub config: ConfigService,
    pub selector: Selector,
    pub http: reqwest::Client,
}

pub struct RoutedRequest {
    pub canonical: CanonicalRequest,
    pub route: SelectedRoute,
    pub key: String,
    pub request_id: Uuid,
    pub start: Instant,
}

pub struct RoutingOutput {
    pub canonical: CanonicalRequest,
    pub route: SelectedRoute,
    pub key: String,
    pub request_id: Uuid,
}

impl Pipeline {
    pub fn new(registry: Arc<ProviderRegistry>, config: ConfigService, http: reqwest::Client) -> Self {
        let selector = Selector::new(registry.clone());
        Self {
            registry,
            config,
            selector,
            http,
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
            self.selector
                .route_explicit(&m)
                .ok_or_else(|| AppError::BadRequest(format!("unknown provider in model ref: {m}")))?
        } else {
            self.selector
                .route_tier(&cfg, tier)
                .ok_or_else(|| {
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
            let cfg_key = cfg
                .providers
                .iter()
                .find(|p| p.id == route.provider_id)
                .and_then(|p| p.key.as_deref());
            resolve_key(&route.provider_id, cfg_key)
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
        })
    }

    pub async fn complete(&self, routed: RoutingOutput) -> AppResult<CanonicalResponse> {
        let adapter = self
            .registry
            .get(&routed.route.provider_id)
            .ok_or_else(|| AppError::Internal(format!("provider disappeared: {}", routed.route.provider_id)))?;
        adapter
            .complete(&routed.canonical, &routed.key, &self.http)
            .await
    }

    pub async fn stream(
        &self,
        routed: RoutingOutput,
    ) -> AppResult<super::super::providers::ProviderStream> {
        let adapter = self
            .registry
            .get(&routed.route.provider_id)
            .ok_or_else(|| AppError::Internal(format!("provider disappeared: {}", routed.route.provider_id)))?;
        adapter
            .stream(&routed.canonical, &routed.key, &self.http)
            .await
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
