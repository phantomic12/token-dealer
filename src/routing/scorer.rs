//! Tier assignment. The MVP scorer's job is just to pick a tier from
//! the client-supplied signals. The 4-layer pipeline (heuristics →
//! user rules → classifier model → default) from the design doc lives
//! here as a v2 extension point; for now we honor:
//!   1. `X-Router-Tier: <tier>` request header
//!   2. `tier/model` model string (e.g. `complex/anthropic/claude-opus-4-5`)
//!   3. Heuristic floors (tools → standard+, image → multimodal)
//!   4. Configured default

use super::super::config::ConfigService;
use super::super::schema::canonical::{CanonicalRequest, Tier};
use super::super::schema::inbound::InboundRequest;

#[derive(Clone)]
pub struct Scorer {
    config: ConfigService,
}

pub struct ScoringContext<'a> {
    pub inbound: &'a InboundRequest,
    pub headers: &'a axum::http::HeaderMap,
}

impl Scorer {
    pub fn new(config: ConfigService) -> Self {
        Self { config }
    }

    /// Returns (tier, optional override_model). `override_model` is
    /// non-None when the request specifies a model directly via
    /// `provider/model` notation — caller uses it to skip tier lookup.
    pub async fn score(&self, ctx: ScoringContext<'_>) -> (Tier, Option<String>) {
        // 1. Explicit header wins.
        if let Some(h) = ctx.headers.get("x-router-tier") {
            if let Ok(s) = h.to_str() {
                if let Some(t) = Tier::parse(s) {
                    return (t, None);
                }
            }
        }

        // 2. tier/model syntax in the model field.
        if let Some((tier_str, model)) = ctx.inbound.model.split_once('/') {
            // Could be `tier/provider/model` — three segments.
            let parts: Vec<&str> = model.splitn(2, '/').collect();
            if let Some(t) = Tier::parse(tier_str) {
                if !parts.is_empty() && !parts[0].is_empty() {
                    return (t, Some(model.to_string()));
                }
            }
        }

        // 3. Heuristic floors.
        let tools_present = ctx
            .inbound
            .tools
            .as_ref()
            .map(|t| !t.is_empty())
            .unwrap_or(false);
        let has_image = ctx.inbound.messages.iter().any(|m| {
            m.content
                .as_array()
                .map(|arr| {
                    arr.iter().any(|p| {
                        matches!(
                            p.get("type").and_then(|t| t.as_str()),
                            Some("image_url") | Some("image")
                        )
                    })
                })
                .unwrap_or(false)
        });
        if has_image {
            return (Tier::Multimodal, None);
        }
        if tools_present {
            return (Tier::Standard, None);
        }

        // 4. Default from config.
        let cfg = self.config.snapshot().await;
        let t = cfg
            .detection
            .default_tier
            .as_deref()
            .and_then(Tier::parse)
            .unwrap_or(Tier::Standard);
        (t, None)
    }

    /// Apply a tier override to an in-progress canonical request. Used
    /// when the scorer has been bypassed (e.g. `tier/model` syntax) and
    /// the caller still wants to record the tier in headers.
    pub fn apply(canonical: &mut CanonicalRequest, tier: Tier) {
        canonical.tier = tier;
    }
}
