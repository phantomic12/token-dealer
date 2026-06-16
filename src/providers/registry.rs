//! Provider registry. Holds the configured adapters and resolves
//! `model_id` lookups. Built once at startup; cheap to clone via Arc.

use std::collections::HashMap;
use std::sync::Arc;

use super::adapter::ProviderAdapter;
use crate::config::types::{ProviderConfig, ProviderType};
use crate::providers::adapters::{AnthropicAdapter, GenericAdapter, OpenAiAdapter};

pub struct ProviderRegistry {
    /// provider_id → adapter
    providers: HashMap<String, Arc<dyn ProviderAdapter>>,
}

impl ProviderRegistry {
    pub fn from_configs(configs: &[ProviderConfig]) -> anyhow::Result<Self> {
        let mut providers = HashMap::new();
        for cfg in configs {
            let key = resolve_key(&cfg.id, cfg.key.as_deref());
            let adapter: Arc<dyn ProviderAdapter> = match cfg.provider_type {
                ProviderType::Anthropic => Arc::new(AnthropicAdapter::new(
                    &cfg.id,
                    &cfg.base_url,
                    cfg.default_model
                        .clone()
                        .unwrap_or_else(|| "claude-sonnet-4-5".to_string()),
                )),
                ProviderType::Openai => Arc::new(OpenAiAdapter::new(
                    &cfg.id,
                    &cfg.base_url,
                    cfg.default_model
                        .clone()
                        .unwrap_or_else(|| "gpt-4o".to_string()),
                )),
                ProviderType::Google => Arc::new(OpenAiAdapter::new(
                    &cfg.id,
                    &cfg.base_url,
                    cfg.default_model
                        .clone()
                        .unwrap_or_else(|| "gemini-2.0-flash".to_string()),
                )),
                ProviderType::Generic => Arc::new(GenericAdapter::new(
                    &cfg.id,
                    &cfg.base_url,
                    cfg.default_model.clone().unwrap_or_else(|| "default".to_string()),
                )),
            };
            // warm key resolution log
            if key.is_empty() {
                tracing::warn!(provider = %cfg.id, "no API key configured; requests to this provider will fail");
            } else {
                tracing::info!(provider = %cfg.id, "registered (key len = {})", key.len());
            }
            providers.insert(cfg.id.clone(), adapter);
        }
        Ok(Self { providers })
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn ProviderAdapter>> {
        self.providers.get(id).cloned()
    }

    pub fn ids(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
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
