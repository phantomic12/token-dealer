//! Tier assignment. 4-layer pipeline:
//!   1. X-Router-Tier header  (explicit override)
//!   2. `tier/provider/model` syntax in the model field
//!   3. Heuristics: image → multimodal, tools → standard, large
//!      context → high_context, formal-logic keywords → reasoning,
//!      code blocks → complex candidate
//!   4. User-defined `[[detection.rules]]`
//!   5. Configured `default_tier`
//!
//! The scorer is async because tier 2 + tier 3 inspect the registry.

use super::super::config::ConfigService;
use super::super::schema::canonical::{CanonicalRequest, Tier};
use super::super::schema::inbound::InboundRequest;

const HIGH_CONTEXT_TOKENS: u32 = 50_000;
const REASONING_KEYWORDS: &[&str] = &[
    "prove", "proof", "formally", "theorem", "lemma", "axiom",
    "derive", "deduce", "step by step", "chain of thought",
    "mathematically", "logically valid", "sound and complete",
];

const CODE_FENCE: &str = "```";

#[derive(Clone)]
pub struct Scorer {
    pub config: ConfigService,
}

pub struct ScoringContext<'a> {
    pub inbound: &'a InboundRequest,
    pub headers: &'a axum::http::HeaderMap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoreResult {
    pub tier: Tier,
    /// Set when the model field was `tier/provider/model` or `provider/model`
    /// — bypasses tier lookup.
    pub model_override: Option<String>,
}

impl Scorer {
    pub fn new(config: ConfigService) -> Self {
        Self { config }
    }

    pub async fn score(&self, ctx: ScoringContext<'_>) -> ScoreResult {
        // 1. Explicit header
        if let Some(h) = ctx.headers.get("x-router-tier") {
            if let Ok(s) = h.to_str() {
                if let Some(t) = Tier::parse(s) {
                    return ScoreResult { tier: t, model_override: None };
                }
            }
        }

        // 2. `tier/provider/model` syntax → set both tier and override
        let parts: Vec<&str> = ctx.inbound.model.splitn(3, '/').collect();
        if parts.len() == 3 {
            if let Some(t) = Tier::parse(parts[0]) {
                return ScoreResult {
                    tier: t,
                    model_override: Some(ctx.inbound.model.clone()),
                };
            }
        }
        // `provider/model` (no tier prefix) — set override only
        if parts.len() == 2 {
            if resolve_alias_lite(parts[0]).is_some() {
                return ScoreResult {
                    tier: Tier::Standard, // best-guess; selector validates
                    model_override: Some(ctx.inbound.model.clone()),
                };
            }
        }

        // 3 + 4. Heuristics + user rules
        let cfg = self.config.snapshot().await;

        // Tier floors from features (these force UP only)
        let tools_present = ctx
            .inbound
            .tools
            .as_ref()
            .map(|t| !t.is_empty())
            .unwrap_or(false);
        let has_image = has_image_in_messages(&ctx.inbound.messages);
        let has_code = has_code_fence(&ctx.inbound.messages);
        let mut tier = if has_image {
            Tier::Multimodal
        } else if has_reasoning_keywords(&ctx.inbound.messages) {
            Tier::Reasoning
        } else if has_code {
            Tier::Complex
        } else {
            Tier::Standard
        };

        // Tool floor: tools → at least Standard (already the case)
        if tools_present && matches!(tier, Tier::Simple) {
            tier = Tier::Standard;
        }

        // Context-size floor: large messages → high_context
        let approx_tokens = approx_token_count(&ctx.inbound.messages);
        if approx_tokens > HIGH_CONTEXT_TOKENS {
            tier = match tier {
                Tier::Simple | Tier::Standard => Tier::HighContext,
                Tier::Complex | Tier::Reasoning | Tier::Multimodal => tier,
                Tier::HighContext => Tier::HighContext,
            };
        }

        // 4. User rules (in order, first match wins; otherwise floor up)
        for rule in &cfg.detection.rules {
            if rule_matches(&rule.condition, &ctx.inbound, approx_tokens, tools_present, has_image) {
                if let Some(t) = Tier::parse(&rule.tier) {
                    if tier_rank(t) > tier_rank(tier) {
                        tier = t;
                    }
                }
            }
        }

        // 5. Default if heuristic didn't pick anything stronger
        if matches!(tier, Tier::Standard) {
            if let Some(d) = cfg.detection.default_tier.as_deref() {
                if let Some(t) = Tier::parse(d) {
                    tier = t;
                }
            }
        }

        ScoreResult { tier, model_override: None }
    }
}

fn tier_rank(t: Tier) -> u8 {
    match t {
        Tier::Simple => 1,
        Tier::Standard => 2,
        Tier::Complex => 3,
        Tier::Reasoning => 4,
        Tier::Multimodal => 5,
        Tier::HighContext => 6,
    }
}

fn has_image_in_messages(messages: &[crate::schema::inbound::InboundMessage]) -> bool {
    messages.iter().any(|m| match &m.content {
        serde_json::Value::Array(arr) => arr.iter().any(|p| {
            matches!(
                p.get("type").and_then(|t| t.as_str()),
                Some("image_url") | Some("image")
            )
        }),
        _ => false,
    })
}

fn has_code_fence(messages: &[crate::schema::inbound::InboundMessage]) -> bool {
    messages.iter().any(|m| {
        let text = match &m.content {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(arr) => arr
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        text.contains(CODE_FENCE)
    })
}

fn has_reasoning_keywords(messages: &[crate::schema::inbound::InboundMessage]) -> bool {
    let combined: String = messages
        .iter()
        .filter(|m| m.role == "user")
        .filter_map(|m| match &m.content {
            serde_json::Value::String(s) => Some(s.to_lowercase()),
            serde_json::Value::Array(arr) => Some(
                arr.iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .map(|s| s.to_lowercase())
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    REASONING_KEYWORDS
        .iter()
        .any(|kw| combined.contains(kw))
}

/// Rough token estimate: ~4 chars per token for English. Good enough
/// for tier classification; not a substitute for real tokenization
/// (which tiktoken-rs would provide, added in phase 2).
fn approx_token_count(messages: &[crate::schema::inbound::InboundMessage]) -> u32 {
    let chars: usize = messages
        .iter()
        .map(|m| match &m.content {
            serde_json::Value::String(s) => s.len(),
            serde_json::Value::Array(arr) => arr
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .map(|s| s.len())
                .sum::<usize>(),
            _ => 0,
        })
        .sum();
    (chars / 4) as u32
}

fn rule_matches(
    cond: &crate::config::types::DetectionCondition,
    inbound: &InboundRequest,
    approx_tokens: u32,
    tools_present: bool,
    has_image: bool,
) -> bool {
    if let Some(want) = cond.has_tools {
        if want != tools_present {
            return false;
        }
    }
    if let Some(threshold) = cond.input_tokens_gt {
        if approx_tokens <= threshold {
            return false;
        }
    }
    if let Some(keywords) = &cond.prompt_contains {
        let combined: String = inbound
            .messages
            .iter()
            .filter_map(|m| match &m.content {
                serde_json::Value::String(s) => Some(s.to_lowercase()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        if !keywords.iter().all(|kw| combined.contains(&kw.to_lowercase())) {
            return false;
        }
    }
    if has_image && cond.has_tools.is_none() && cond.input_tokens_gt.is_none()
        && cond.prompt_contains.is_none()
    {
        // Empty condition matches everything; skip the false positive.
    }
    let _ = has_image;
    true
}

fn resolve_alias_lite(s: &str) -> Option<()> {
    use super::super::providers::resolve_alias;
    if resolve_alias(s).is_some() {
        Some(())
    } else {
        None
    }
}
