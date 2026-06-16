//! Provider registry. Holds the configured adapters and resolves
//! `model_id` lookups. Mutable via an `RwLock` so the admin UI can
//! add/remove providers at runtime; on startup it's seeded from the
//! TOML config.
//!
//! `from_configs` is the single place that maps a `ProviderType` to an
//! adapter. Adding a new provider = one row in `manifest::lookup` +
//! one match arm here (or none, if it goes through the OpenAI path).

use super::adapter::ProviderAdapter;
use super::adapters::{
    AnthropicAdapter, GenericAdapter, GoogleAdapter, KiroAdapter, OpenAiAdapter, ResponsesAdapter,
};
use super::manifest;
use crate::config::types::{ProviderConfig, ProviderType};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct ProviderRegistry {
    providers: Arc<RwLock<HashMap<String, Arc<dyn ProviderAdapter>>>>,
}

impl ProviderRegistry {
    pub fn from_configs(configs: &[ProviderConfig]) -> anyhow::Result<Self> {
        let mut map = HashMap::new();
        for cfg in configs {
            let key = resolve_key(&cfg.id, cfg.key.as_deref());
            let adapter = build_adapter(cfg)?;
            if key.is_empty() {
                tracing::warn!(provider = %cfg.id, "no API key configured; requests to this provider will fail");
            } else {
                tracing::info!(provider = %cfg.id, "registered (key len = {})", key.len());
            }
            map.insert(cfg.id.clone(), adapter);
        }
        Ok(Self {
            providers: Arc::new(RwLock::new(map)),
        })
    }

    /// Read-only clone of the current provider list (just the IDs).
    pub async fn ids(&self) -> Vec<String> {
        self.providers.read().await.keys().cloned().collect()
    }

    /// Read-only handle to a single provider. Clones the Arc.
    pub async fn get(&self, id: &str) -> Option<Arc<dyn ProviderAdapter>> {
        self.providers.read().await.get(id).cloned()
    }

    /// Snapshot of every (id, default_model) pair for `/v1/models`.
    pub async fn list(&self) -> Vec<(String, String)> {
        let g = self.providers.read().await;
        g.iter()
            .map(|(id, adapter)| (id.clone(), adapter.default_model().to_string()))
            .collect()
    }

    /// Live add. Used by the admin UI and POST /admin/providers.
    pub async fn add(&self, cfg: &ProviderConfig) -> anyhow::Result<()> {
        let adapter = build_adapter(cfg)?;
        let key = resolve_key(&cfg.id, cfg.key.as_deref());
        if key.is_empty() {
            tracing::warn!(provider = %cfg.id, "added with no API key");
        } else {
            tracing::info!(provider = %cfg.id, "added (key len = {})", key.len());
        }
        self.providers.write().await.insert(cfg.id.clone(), adapter);
        Ok(())
    }

    /// Live remove. Returns true if a provider was actually removed.
    pub async fn remove(&self, id: &str) -> bool {
        let mut g = self.providers.write().await;
        let removed = g.remove(id).is_some();
        if removed {
            tracing::info!(provider = %id, "removed");
        }
        removed
    }

    /// Resolve `provider/model` notation. Returns (provider_id, model_id).
    /// If `model_str` doesn't contain a `/`, returns None so the caller
    /// can fall back to the tier-based primary.
    pub fn split_model_ref(model_str: &str) -> Option<(String, String)> {
        let (p, m) = model_str.split_once('/')?;
        if p.is_empty() || m.is_empty() {
            return None;
        }
        Some((p.to_string(), m.to_string()))
    }
}

fn build_adapter(cfg: &ProviderConfig) -> anyhow::Result<Arc<dyn ProviderAdapter>> {
    let meta = manifest::lookup(cfg.provider_type);
    let base_url = cfg
        .base_url
        .clone()
        .or_else(|| meta.map(|m| m.base_url.to_string()))
        .ok_or_else(|| {
            anyhow::anyhow!("provider {} has no base_url (use a non-Generic type)", cfg.id)
        })?;
    let default_model = cfg
        .default_model
        .clone()
        .or_else(|| meta.map(|m| m.default_model.to_string()))
        .unwrap_or_else(|| "default".to_string());
    let path = cfg
        .path
        .clone()
        .or_else(|| meta.map(|m| m.path.to_string()))
        .unwrap_or_else(|| "/v1/chat/completions".to_string());

    Ok(match cfg.provider_type {
        ProviderType::Anthropic => Arc::new(AnthropicAdapter::new(
            &cfg.id,
            base_url,
            default_model,
        )),
        ProviderType::Google => Arc::new(GoogleAdapter::new(&cfg.id, base_url, default_model)),
        ProviderType::Kiro => Arc::new(KiroAdapter::new(&cfg.id, base_url, default_model)),
        ProviderType::Responses => {
            Arc::new(ResponsesAdapter::new(&cfg.id, base_url, default_model))
        }
        ProviderType::Generic => Arc::new(GenericAdapter::new(&cfg.id, base_url, default_model)),
        _ => Arc::new(OpenAiAdapter::with_path(
            &cfg.id,
            base_url,
            path,
            default_model,
        )),
    })
}

/// Resolves the API key for a provider: env var first, then literal.
/// Returns the literal value, possibly empty.
pub fn resolve_key(provider_id: &str, literal: Option<&str>) -> String {
    if let Some(lit) = literal {
        if let Some(inner) = lit.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
            if let Ok(v) = std::env::var(inner) {
                return v;
            }
        } else if !lit.is_empty() {
            return lit.to_string();
        }
    }
    let env_var = format!("{}_API_KEY", provider_id.to_uppercase().replace('-', "_"));
    std::env::var(&env_var).unwrap_or_default()
}
