//! Model selection: given a tier and a `provider/model` (or just a
//! `tier`-level primary), resolve to a (provider_id, model_id) pair
//! and an adapter from the registry.

use super::super::config::RouterConfig;
use super::super::providers::ProviderRegistry;
use super::super::schema::canonical::Tier;
use std::sync::Arc;

#[derive(Clone)]
pub struct Selector {
    registry: Arc<ProviderRegistry>,
}

#[derive(Debug, Clone)]
pub struct SelectedRoute {
    pub provider_id: String,
    pub model_id: String,
}

impl Selector {
    pub fn new(registry: Arc<ProviderRegistry>) -> Self {
        Self { registry }
    }

    /// Resolve a fully-qualified `provider/model` string.
    pub async fn route_explicit(&self, model_ref: &str) -> Option<SelectedRoute> {
        let (p, m) = ProviderRegistry::split_model_ref(model_ref)?;
        if self.registry.get(&p).await.is_none() {
            return None;
        }
        Some(SelectedRoute {
            provider_id: p,
            model_id: m,
        })
    }

    /// Resolve a tier to the configured primary model.
    pub async fn route_tier(&self, config: &RouterConfig, tier: Tier) -> Option<SelectedRoute> {
        let primary = config.primary_for_tier(tier)?;
        self.route_explicit(primary).await
    }
}
