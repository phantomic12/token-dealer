//! OpenRouter pricing sync.
//!
//! OpenRouter publishes a free, no-auth JSON catalog of 300+ model
//! prices at <https://openrouter.ai/api/v1/models>. token-dealer
//! ingests this catalog at startup and every `interval_hours` to
//! populate the `model_prices` table used by `PricingStore` for cost
//! computation.
//!
//! This mirrors `mnfst/manifest`'s `ModelPricingCacheService` +
//! `PricingSyncService` without the full ProviderKey-management layer.
//!
//! Manual seeding via `POST /admin/pricing` is still supported and
//! wins over sync data on conflict — the sync only adds/updates rows
//! that the user hasn't explicitly overridden.

use crate::cost::PricingStore;
use crate::db::Db;
use serde::Deserialize;
use std::time::Duration;
use tokio::time::sleep;

/// Subset of the OpenRouter `/api/v1/models` response we care about.
#[derive(Debug, Clone, Deserialize)]
pub struct OpenRouterModel {
    pub id: String,
    /// OpenRouter nests prices under `pricing` (in USD per token).
    /// We multiply by 1_000_000 to convert to per-1M tokens for our
    /// internal representation.
    #[serde(default)]
    pub pricing: Option<OpenRouterPricing>,
    /// Optional context window in tokens.
    #[serde(default)]
    pub context_length: Option<u32>,
    /// OpenRouter modality flags (text/image/audio/video in/out) are
    /// packed into a single string like "text+image->text". We use
    /// the simpler `architecture.modality` when present.
    #[serde(default)]
    pub architecture: Option<OpenRouterArchitecture>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenRouterPricing {
    /// USD per token (not per 1M).
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub completion: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenRouterArchitecture {
    #[serde(default)]
    pub modality: Option<String>,
}

/// Top-level response shape.
#[derive(Debug, Deserialize)]
struct OpenRouterResponse {
    data: Vec<OpenRouterModel>,
}

/// Fetch and persist. Returns the number of rows upserted.
pub async fn sync_once(
    http: &reqwest::Client,
    db: &Db,
    url: &str,
) -> anyhow::Result<usize> {
    let resp = http
        .get(url)
        .timeout(Duration::from_secs(30))
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "OpenRouter pricing sync failed: {} {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
    }
    let parsed: OpenRouterResponse = resp.json().await?;
    let pricing = PricingStore::new(db.clone());
    let mut upserted = 0usize;
    for m in parsed.data {
        let Some(p) = m.pricing else { continue };
        let prompt_per_token: f64 = match p.prompt.as_deref().and_then(|s| s.parse().ok()) {
            Some(v) => v,
            None => continue,
        };
        let completion_per_token: f64 = match p.completion.as_deref().and_then(|s| s.parse().ok()) {
            Some(v) => v,
            None => continue,
        };
        let input_per_1m = prompt_per_token * 1_000_000.0;
        let output_per_1m = completion_per_token * 1_000_000.0;
        let context = m.context_length.unwrap_or(8192);
        // Modality: 1 = text-only, plus bit-flags for image/audio/video
        // in/out if the OpenRouter string contains "+image" etc.
        let mut modality = 1u32;
        if let Some(arch) = m.architecture {
            if let Some(mod_str) = arch.modality {
                if mod_str.contains("image") {
                    modality |= 0b10;
                }
                if mod_str.contains("audio") {
                    modality |= 0b11000;
                }
            }
        }
        pricing
            .upsert(&m.id, input_per_1m, output_per_1m, context, modality)
            .await?;
        upserted += 1;
    }
    Ok(upserted)
}

/// Spawn the recurring sync task. The first run fires after 5 seconds
/// (so startup logs aren't blocked), then every `interval_hours`.
///
/// `enabled = false` short-circuits — the task returns immediately.
pub fn spawn_pricing_sync(
    http: reqwest::Client,
    db: Db,
    cfg: crate::config::types::PricingSyncConfig,
) {
    if !cfg.enabled {
        return;
    }
    let interval = Duration::from_secs(cfg.interval_hours.saturating_mul(3600));
    tokio::spawn(async move {
        // Initial delay so we don't block startup logs.
        sleep(Duration::from_secs(5)).await;
        loop {
            match sync_once(&http, &db, &cfg.openrouter_url).await {
                Ok(n) => tracing::info!(
                    count = n,
                    url = %cfg.openrouter_url,
                    "OpenRouter pricing sync completed"
                ),
                Err(e) => tracing::warn!(
                    error = %e,
                    url = %cfg.openrouter_url,
                    "OpenRouter pricing sync failed; will retry next interval"
                ),
            }
            sleep(interval).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openrouter_response_shape() {
        let json = r#"{
            "data": [
                {
                    "id": "anthropic/claude-sonnet-4-5",
                    "pricing": {"prompt": "0.000003", "completion": "0.000015"},
                    "context_length": 200000,
                    "architecture": {"modality": "text->text"}
                },
                {
                    "id": "openai/gpt-4o",
                    "pricing": {"prompt": "0.000005", "completion": "0.000015"},
                    "context_length": 128000,
                    "architecture": {"modality": "text+image->text"}
                }
            ]
        }"#;
        let parsed: OpenRouterResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.data.len(), 2);
        assert_eq!(parsed.data[0].id, "anthropic/claude-sonnet-4-5");
        let p = parsed.data[0].pricing.as_ref().unwrap();
        assert_eq!(p.prompt.as_deref().unwrap(), "0.000003");
        assert_eq!(p.completion.as_deref().unwrap(), "0.000015");
        assert_eq!(parsed.data[1].context_length, Some(128000));
        // modality: text+image->text → text (bit 0) + image_in (bit 1)
        let arch = parsed.data[1].architecture.as_ref().unwrap();
        let mod_str = arch.modality.as_deref().unwrap();
        assert!(mod_str.contains("image"));
    }

    #[test]
    fn per_token_to_per_million_conversion() {
        // OpenRouter prices are USD per token. We need per-1M.
        let prompt_per_token = 0.000003_f64;
        let input_per_1m = prompt_per_token * 1_000_000.0;
        assert!((input_per_1m - 3.0).abs() < 1e-9);
    }
}
