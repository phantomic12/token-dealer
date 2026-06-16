//! Cost calculation. `models.dev` sync is phase 2; for now we use the
//! manifest metadata (which mirrors what manifest knows) and the
//! user-overrides table from the cost config.

use crate::providers::manifest;
use crate::tokens;

pub fn calculate(
    provider_id: &str,
    model_id: &str,
    input_tokens: u32,
    output_tokens: u32,
) -> Option<f64> {
    // No canonical registry per-provider-model yet (no models.dev
    // sync). For now, we estimate via the manifest default model
    // for the provider. If a user provides overrides in the cost
    // table, we use those.
    let meta = manifest_lookup_by_id(provider_id)?;
    let is_default = meta.default_model == model_id;
    let (in_price, out_price) = if is_default {
        (default_price_in(provider_id), default_price_out(provider_id))
    } else {
        // Conservative: treat non-default models as "tier-2" pricing
        // 2x default. Better than nothing for an estimate.
        (default_price_in(provider_id) * 2.0, default_price_out(provider_id) * 2.0)
    };
    let cost = (input_tokens as f64 / 1_000_000.0) * in_price
        + (output_tokens as f64 / 1_000_000.0) * out_price;
    Some(cost)
}

/// Estimate token count from raw text using tiktoken-rs. Used by the
/// scorer for high_context detection and by callers that need a
/// token count before the response comes back.
pub fn estimate_tokens(model: &str, text: &str) -> u32 {
    tokens::count(model, text)
}

fn manifest_lookup_by_id(provider_id: &str) -> Option<manifest::ManifestProvider> {
    use crate::providers::resolve_alias;
    let pt = resolve_alias(provider_id)?;
    manifest::lookup(pt)
}

/// Hard-coded prices for the providers we know well, in USD per
/// million tokens (input, output). Conservative defaults for unknown
/// providers. Replace with a proper models.dev sync in phase 2.
fn default_price_in(provider_id: &str) -> f64 {
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
