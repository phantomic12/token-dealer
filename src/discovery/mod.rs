//! Model discovery — fetch `/v1/models` from each connected provider
//! on startup and cache the model list per provider in the
//! `provider_models` SQLite table. Powers:
//!   1. The `/v1/models` OpenAI-compatible listing (when enabled,
//!      returns discovered models instead of just the default_model)
//!   2. Tier auto-assignment — pick the cheapest model with
//!      sufficient quality for each tier from the discovered pool
//!
//! Mirrors `mnfst/manifest`'s `ProviderModelFetcherService` but in
//! one file. The fetch path lives in `ProviderAdapter::list_models`
//! already, so this module is just the cache + orchestrator.

use crate::db::Db;
use crate::providers::ProviderRegistry;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize)]
pub struct ProviderModelRow {
    pub provider_id: String,
    pub model_id: String,
}

/// Discover models for every registered provider and upsert into
/// the `provider_models` table. Returns the number of rows upserted.
///
/// Failures on individual providers are logged + skipped — a single
/// provider being down shouldn't break the whole startup.
pub async fn discover_all(
    db: &Db,
    registry: &ProviderRegistry,
    http: &reqwest::Client,
    key_store: &crate::auth::KeyStore,
) -> anyhow::Result<usize> {
    let mut upserted = 0usize;
    let providers = registry.ids().await;
    for pid in providers {
        let adapter = match registry.get(&pid).await {
            Some(a) => a,
            None => continue,
        };
        let key = match key_store.get(&pid).await {
            Ok(Some(k)) => k,
            _ => String::new(),
        };
        let models = match adapter.list_models(&key, http).await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    provider = %pid,
                    error = %e,
                    "model discovery failed for provider; skipping"
                );
                continue;
            }
        };
        let count = upsert_for_provider(db, &pid, models).await?;
        upserted += count;
    }
    Ok(upserted)
}

async fn upsert_for_provider(
    db: &Db,
    provider_id: &str,
    models: Vec<String>,
) -> anyhow::Result<usize> {
    let pid = provider_id.to_string();
    let count = models.len();
    db.with(move |c| {
        c.execute(
            "DELETE FROM provider_models WHERE provider_id = ?1",
            rusqlite::params![&pid],
        )?;
        for m in &models {
            c.execute(
                "INSERT INTO provider_models (provider_id, model_id, discovered_at)
                 VALUES (?1, ?2, CURRENT_TIMESTAMP)
                 ON CONFLICT(provider_id, model_id) DO UPDATE SET
                   discovered_at = CURRENT_TIMESTAMP",
                rusqlite::params![&pid, m],
            )?;
        }
        Ok(())
    })
    .await?;
    Ok(count)
}

/// List all discovered models, optionally filtered by provider.
pub async fn list_discovered(
    db: &Db,
    provider_id: Option<&str>,
) -> anyhow::Result<Vec<ProviderModelRow>> {
    let pid_filter = provider_id.map(|s| s.to_string());
    db.with(move |c| {
        let (sql, args): (&str, Vec<String>) = match &pid_filter {
            Some(p) => (
                "SELECT provider_id, model_id FROM provider_models WHERE provider_id = ?1 ORDER BY provider_id, model_id",
                vec![p.clone()],
            ),
            None => (
                "SELECT provider_id, model_id FROM provider_models ORDER BY provider_id, model_id",
                vec![],
            ),
        };
        let mut stmt = c.prepare(sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args.iter()), |r| {
                Ok(ProviderModelRow {
                    provider_id: r.get(0)?,
                    model_id: r.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

/// Build the migration row for `provider_models`. Idempotent.
pub fn run_migration(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS provider_models (
            provider_id TEXT NOT NULL,
            model_id    TEXT NOT NULL,
            discovered_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (provider_id, model_id)
        );
        CREATE INDEX IF NOT EXISTS idx_provider_models_provider
            ON provider_models(provider_id);
        "#,
    )
}

/// Spawn the discovery task. Runs once on startup after a 5s delay.
pub fn spawn_discovery(
    db: Db,
    registry: std::sync::Arc<ProviderRegistry>,
    http: reqwest::Client,
    key_store: crate::auth::KeyStore,
) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        match discover_all(&db, &registry, &http, &key_store).await {
            Ok(n) => tracing::info!(count = n, "model discovery completed"),
            Err(e) => tracing::warn!(error = %e, "model discovery failed"),
        }
    });
}

/// Pick the cheapest model per tier from the discovered pool using
/// the pricing store. Returns a map of tier_name → "provider/model".
/// Only fills tiers whose primary is currently empty — won't
/// overwrite user-set primaries.
pub async fn auto_assign_tiers(
    db: &Db,
    pricing: &crate::cost::PricingStore,
    config: &crate::config::RouterConfig,
) -> anyhow::Result<HashMap<String, String>> {
    use crate::schema::canonical::Tier;
    let discovered = list_discovered(db, None).await?;
    let mut out = HashMap::new();
    let mut by_provider: HashMap<String, Vec<String>> = HashMap::new();
    for m in discovered {
        by_provider
            .entry(m.provider_id)
            .or_default()
            .push(m.model_id);
    }
    let tiers = [
        (Tier::Simple, 2.0_f64),
        (Tier::Standard, 6.0),
        (Tier::Complex, 15.0),
        (Tier::Reasoning, 15.0),
        (Tier::HighContext, 6.0),
        (Tier::Multimodal, 4.0),
    ];
    for (tier, max_input) in tiers {
        // Skip tiers that already have a primary
        if config.primary_for_tier(tier).is_some() {
            continue;
        }
        let mut best: Option<(f64, String)> = None;
        for (provider_id, models) in &by_provider {
            for model_id in models {
                let price_row = match pricing.get(model_id).await {
                    Ok(Some(p)) => p,
                    _ => continue,
                };
                if price_row.input_per_1m > max_input || price_row.input_per_1m <= 0.0 {
                    continue;
                }
                let candidate = price_row.input_per_1m;
                match &best {
                    Some((p, _)) if *p <= candidate => {}
                    _ => best = Some((candidate, format!("{provider_id}/{model_id}"))),
                }
            }
        }
        if let Some((_, primary)) = best {
            out.insert(tier.as_str().to_string(), primary);
        }
    }
    Ok(out)
}
