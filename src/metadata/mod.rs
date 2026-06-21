//! Model metadata from models.dev. Background-fetched on startup +
//! re-fetched every 24h. Cached in-memory + persisted to SQLite.
//!
//! Falls back gracefully to the hardcoded manifest table when
//! models.dev is unreachable, so the proxy is never held up by
//! upstream flake.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

const MODELS_DEV_URL: &str = "https://models.dev/api.json";
const REFETCH_INTERVAL: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelMetadata {
    pub provider: String,
    pub model_id: String,
    pub context_window: Option<u32>,
    pub output_limit: Option<u32>,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_audio: bool,
    pub supports_reasoning: bool,
    pub input_per_million: Option<f64>,
    pub output_per_million: Option<f64>,
    pub cache_read_per_million: Option<f64>,
    pub cache_write_per_million: Option<f64>,
    /// "models_dev" | "user_override" | "manifest_default"
    pub source: String,
}

#[derive(Clone, Default)]
pub struct MetadataStore {
    inner: Arc<RwLock<HashMap<(String, String), ModelMetadata>>>,
}

impl MetadataStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get(&self, provider: &str, model: &str) -> Option<ModelMetadata> {
        self.inner
            .read()
            .await
            .get(&(provider.to_string(), model.to_string()))
            .cloned()
    }

    pub async fn put(&self, meta: ModelMetadata) {
        let key = (meta.provider.clone(), meta.model_id.clone());
        self.inner.write().await.insert(key, meta);
    }

    pub async fn all(&self) -> Vec<ModelMetadata> {
        self.inner.read().await.values().cloned().collect()
    }
}

/// Fetch + parse the models.dev JSON. Returns a list of `ModelMetadata`.
/// Caller inserts into the store.
pub async fn fetch_from_models_dev() -> anyhow::Result<Vec<ModelMetadata>> {
    let resp = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?
        .get(MODELS_DEV_URL)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("models.dev returned {}", resp.status());
    }
    let body: serde_json::Value = resp.json().await?;
    let mut out = Vec::new();
    if let Some(obj) = body.as_object() {
        for (provider, models_val) in obj {
            if let Some(models) = models_val.as_object() {
                for (model_id, m) in models {
                    let cost = m.get("cost").cloned().unwrap_or(Value::Null);
                    let limits = m.get("limit").cloned().unwrap_or(Value::Null);
                    let modalities = m.get("modalities").cloned().unwrap_or(Value::Null);
                    let meta = ModelMetadata {
                        provider: provider.clone(),
                        model_id: model_id.clone(),
                        context_window: limits
                            .get("context")
                            .and_then(|v| v.as_u64())
                            .map(|x| x as u32),
                        output_limit: limits
                            .get("output")
                            .and_then(|v| v.as_u64())
                            .map(|x| x as u32),
                        supports_tools: modalities
                            .get("output")
                            .and_then(|v| v.as_array())
                            .map(|arr| arr.iter().any(|s| s.as_str() == Some("tools")))
                            .unwrap_or(false),
                        supports_vision: modalities
                            .get("input")
                            .and_then(|v| v.as_array())
                            .map(|arr| arr.iter().any(|s| s.as_str() == Some("image")))
                            .unwrap_or(false),
                        supports_audio: modalities
                            .get("input")
                            .and_then(|v| v.as_array())
                            .map(|arr| arr.iter().any(|s| s.as_str() == Some("audio")))
                            .unwrap_or(false),
                        supports_reasoning: m
                            .get("reasoning")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        input_per_million: cost.get("input").and_then(|v| v.as_f64()),
                        output_per_million: cost.get("output").and_then(|v| v.as_f64()),
                        cache_read_per_million: cost.get("cache_read").and_then(|v| v.as_f64()),
                        cache_write_per_million: cost.get("cache_write").and_then(|v| v.as_f64()),
                        source: "models_dev".to_string(),
                    };
                    out.push(meta);
                }
            }
        }
    }
    Ok(out)
}

/// Spawn a background task that periodically refreshes the store.
/// Returns immediately. First fetch happens in the background so
/// startup isn't blocked.
pub fn spawn_refresher(store: MetadataStore) {
    tokio::spawn(async move {
        loop {
            match fetch_from_models_dev().await {
                Ok(metas) => {
                    let n = metas.len();
                    for m in metas {
                        store.put(m).await;
                    }
                    tracing::info!(count = n, "models.dev metadata refreshed");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "models.dev fetch failed; using cached/manifest defaults");
                }
            }
            tokio::time::sleep(REFETCH_INTERVAL).await;
        }
    });
}
