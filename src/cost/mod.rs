//! Cost calculation.
//!
//! `PricingStore` is the DB-backed model price table. `calculate()`
//! resolves a (provider_id, model_id) pair to a per-request cost
//! using:
//!   1. The exact `model_prices` row (if seeded from OpenRouter sync
//!      or user-overridden)
//!   2. The provider's default-model price (from `default_price_*`)
//!   3. A conservative fallback for unknown providers
//!
//! All prices are in USD per 1M tokens.

use crate::db::Db;
use crate::providers::manifest;
use crate::providers::resolve_alias;
use crate::tokens;

pub mod limits;
pub mod openrouter_sync;

pub use openrouter_sync::{spawn_pricing_sync, sync_once};

/// DB-backed pricing store. Holds the `model_prices` table and
/// provides per-(provider, model) cost lookups.
#[derive(Clone)]
pub struct PricingStore {
    db: Db,
}

impl PricingStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn upsert(
        &self,
        model_id: &str,
        input_per_1m: f64,
        output_per_1m: f64,
        context_window: u32,
        modality: u32,
    ) -> anyhow::Result<()> {
        let model_id = model_id.to_string();
        self.db
            .with(move |c| {
                c.execute(
                    "INSERT INTO model_prices (model_id, input_per_1k, output_per_1k, context_window, modality)
                     VALUES (?, ?, ?, ?, ?)
                     ON CONFLICT(model_id) DO UPDATE SET
                       input_per_1k = excluded.input_per_1k,
                       output_per_1k = excluded.output_per_1k,
                       context_window = excluded.context_window,
                       modality = excluded.modality,
                       updated_at = CURRENT_TIMESTAMP",
                    rusqlite::params![
                        model_id,
                        input_per_1m / 1000.0, // store per-1k, take per-1m
                        output_per_1m / 1000.0,
                        context_window as i64,
                        modality as i64,
                    ],
                )?;
                Ok(())
            })
            .await
    }

    pub async fn get(&self, model_id: &str) -> anyhow::Result<Option<ModelPrice>> {
        let model_id = model_id.to_string();
        self.db
            .with(move |c| {
                let mut stmt = c.prepare(
                    "SELECT model_id, input_per_1k, output_per_1k, context_window, modality
                     FROM model_prices WHERE model_id = ?",
                )?;
                let mut rows = stmt.query([&model_id])?;
                if let Some(row) = rows.next()? {
                    let model_id: String = row.get(0)?;
                    let in_p: f64 = row.get(1)?;
                    let out_p: f64 = row.get(2)?;
                    let ctx: i64 = row.get(3)?;
                    let mod_: i64 = row.get(4)?;
                    Ok(Some(ModelPrice {
                        model_id,
                        input_per_1m: in_p * 1000.0,
                        output_per_1m: out_p * 1000.0,
                        context_window: ctx as u32,
                        modality: mod_ as u32,
                    }))
                } else {
                    Ok(None)
                }
            })
            .await
    }

    pub async fn list(&self) -> anyhow::Result<Vec<ModelPrice>> {
        self.db
            .with(|c| {
                let mut stmt = c.prepare(
                    "SELECT model_id, input_per_1k, output_per_1k, context_window, modality
                     FROM model_prices ORDER BY model_id",
                )?;
                let rows = stmt
                    .query_map([], |row| {
                        let model_id: String = row.get(0)?;
                        let in_p: f64 = row.get(1)?;
                        let out_p: f64 = row.get(2)?;
                        let ctx: i64 = row.get(3)?;
                        let mod_: i64 = row.get(4)?;
                        Ok(ModelPrice {
                            model_id,
                            input_per_1m: in_p * 1000.0,
                            output_per_1m: out_p * 1000.0,
                            context_window: ctx as u32,
                            modality: mod_ as u32,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
    }

    pub async fn seed_defaults(&self) -> anyhow::Result<usize> {
        // Seed the prices we hard-code in `default_price_*`. Run on
        // first start so the DB has SOMETHING even before models.dev
        // sync lands.
        let seeds = default_price_seeds();
        let mut n = 0;
        for (model_id, (in_p, out_p), modality) in seeds {
            if self.get(model_id).await?.is_none() {
                self.upsert(model_id, in_p, out_p, 128_000, modality)
                    .await?;
                n += 1;
            }
        }
        Ok(n)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelPrice {
    pub model_id: String,
    pub input_per_1m: f64,
    pub output_per_1m: f64,
    pub context_window: u32,
    /// Bitfield of capability flags (text=1, image_in=2, image_out=4,
    /// audio_in=8, audio_out=16, video_in=32, video_out=64)
    pub modality: u32,
}

pub fn calculate(
    provider_id: &str,
    model_id: &str,
    input_tokens: u32,
    output_tokens: u32,
) -> Option<f64> {
    calculate_with_db(provider_id, model_id, input_tokens, output_tokens, None)
}

/// Variant that consults a PricingStore first, falls back to the
/// built-in defaults. This is what the pipeline uses.
pub fn calculate_with_db(
    provider_id: &str,
    model_id: &str,
    input_tokens: u32,
    output_tokens: u32,
    pricing: Option<&PricingStore>,
) -> Option<f64> {
    // 1. Try the DB row keyed by the full model id.
    if let Some(p) = pricing.and_then(|s| model_id_to_row(s, model_id).ok().flatten()) {
        let cost = (input_tokens as f64 / 1_000_000.0) * p.input_per_1m
            + (output_tokens as f64 / 1_000_000.0) * p.output_per_1m;
        return Some(cost);
    }
    // 2. Fall back to provider defaults (claude-sonnet-4-5, gpt-4o-mini, etc).
    let meta = manifest_lookup_by_id(provider_id)?;
    let is_default = meta.default_model == model_id;
    let (in_price, out_price) = if is_default {
        (
            default_price_in(provider_id),
            default_price_out(provider_id),
        )
    } else {
        (
            default_price_in(provider_id) * 1.5,
            default_price_out(provider_id) * 1.5,
        )
    };
    let cost = (input_tokens as f64 / 1_000_000.0) * in_price
        + (output_tokens as f64 / 1_000_000.0) * out_price;
    Some(cost)
}

fn model_id_to_row(pricing: &PricingStore, model_id: &str) -> anyhow::Result<Option<ModelPrice>> {
    // Block on the async — this is a small helper called from the
    // sync `calculate_with_db` path. We're inside an async pipeline
    // path; the cost calc is a small overhead.
    futures::executor::block_on(pricing.get(model_id))
}

pub fn estimate_tokens(model: &str, text: &str) -> u32 {
    tokens::count(model, text)
}

fn manifest_lookup_by_id(provider_id: &str) -> Option<manifest::ManifestProvider> {
    let pt = resolve_alias(provider_id)?;
    manifest::lookup(pt)
}

// ── default prices (USD per 1M tokens) ────────────────────────────────
//
// Modality bitfield: text=1, image_in=2, image_out=4, audio_in=8,
// audio_out=16, video_in=32, video_out=64. Use bit-OR to combine.
// "vision" models = text | image_in (3).
fn default_price_seeds() -> Vec<(&'static str, (f64, f64), u32)> {
    vec![
        // Anthropic
        ("anthropic/claude-sonnet-4-5", (3.0, 15.0), 1),
        ("anthropic/claude-opus-4-5", (15.0, 75.0), 1),
        ("anthropic/claude-haiku-4-5", (1.0, 5.0), 1),
        // OpenAI
        ("openai/gpt-4o", (2.5, 10.0), 3), // vision-capable
        ("openai/gpt-4o-mini", (0.15, 0.6), 3),
        ("openai/o3", (10.0, 40.0), 1),
        ("openai/o3-mini", (1.1, 4.4), 1),
        ("openai/o1", (15.0, 60.0), 1),
        ("openai/o1-mini", (3.0, 12.0), 1),
        ("openai/gpt-4.1", (2.0, 8.0), 1),
        ("openai/gpt-4.1-mini", (0.4, 1.6), 1),
        ("openai/gpt-4.1-nano", (0.1, 0.4), 1),
        // Google Gemini
        ("google/gemini-2.0-flash", (0.10, 0.40), 3),
        ("google/gemini-2.5-pro", (1.25, 10.0), 3),
        ("google/gemini-2.5-flash", (0.075, 0.30), 3),
        ("google/gemini-1.5-pro", (1.25, 5.0), 3),
        ("google/gemini-1.5-flash", (0.075, 0.30), 3),
        // DeepSeek
        ("deepseek/deepseek-chat", (0.27, 1.10), 1),
        ("deepseek/deepseek-coder", (0.27, 1.10), 1),
        // Groq
        ("groq/llama-3.3-70b-versatile", (0.59, 0.79), 1),
        ("groq/llama-3.1-8b-instant", (0.05, 0.08), 1),
        // xAI
        ("xai/grok-2", (2.0, 10.0), 1),
        ("xai/grok-2-mini", (0.20, 1.0), 1),
        // Mistral
        ("mistral/mistral-large-latest", (2.0, 6.0), 1),
        ("mistral/mistral-small-latest", (0.20, 0.60), 1),
        // OpenRouter (passthrough — varies)
        ("openrouter/anthropic/claude-sonnet-4-5", (3.0, 15.0), 1),
        // Fireworks
        (
            "fireworks/accounts/fireworks/models/llama-v3p3-70b-instruct",
            (0.90, 0.90),
            1,
        ),
        // Qwen
        ("qwen/qwen-plus", (0.40, 1.20), 1),
        ("qwen/qwen-max", (2.40, 9.60), 1),
        // Moonshot Kimi
        ("moonshot/kimi-k2-0711-preview", (1.0, 3.0), 1),
        // Z.ai GLM
        ("zai/glm-4.5", (0.50, 1.50), 1),
        ("zai/glm-4.5-air", (0.20, 0.60), 1),
        // GitHub Copilot
        ("github-copilot/gpt-4o", (0.0, 0.0), 1), // included in subscription
        // Kiro
        ("kiro/kiro/claude-sonnet-4-5", (0.0, 0.0), 1),
        // Local (free)
        ("ollama/llama3.1", (0.0, 0.0), 1),
        ("ollama-cloud/llama3.1", (0.0, 0.0), 1),
        ("llamacpp/default", (0.0, 0.0), 1),
        ("lmstudio/default", (0.0, 0.0), 1),
    ]
}

fn default_price_in(provider_id: &str) -> f64 {
    // Conservative per-1M-token price when no model-specific row is
    // available. Picked from the seed defaults above.
    match provider_id {
        "anthropic" => 3.0,
        "openai" => 2.5,
        "google" | "gemini" => 0.10,
        "deepseek" => 0.27,
        "groq" => 0.59,
        "xai" | "grok" => 2.0,
        "mistral" => 2.0,
        "openrouter" => 3.0,
        "tokenrouter" => 2.0,
        "fireworks" => 0.90,
        "qwen" | "alibaba" | "dashscope" => 0.40,
        "moonshot" | "kimi" => 1.0,
        "zai" | "z.ai" => 0.50,
        "ollama" | "ollama-cloud" | "llamacpp" | "lmstudio" => 0.0,
        _ => 2.0,
    }
}

fn default_price_out(provider_id: &str) -> f64 {
    match provider_id {
        "anthropic" => 15.0,
        "openai" => 10.0,
        "google" | "gemini" => 0.40,
        "deepseek" => 1.10,
        "groq" => 0.79,
        "xai" | "grok" => 10.0,
        "mistral" => 6.0,
        "openrouter" => 15.0,
        "tokenrouter" => 10.0,
        "fireworks" => 0.90,
        "qwen" | "alibaba" | "dashscope" => 1.20,
        "moonshot" | "kimi" => 3.0,
        "zai" | "z.ai" => 1.50,
        "ollama" | "ollama-cloud" | "llamacpp" | "lmstudio" => 0.0,
        _ => 10.0,
    }
}
