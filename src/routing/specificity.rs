//! Specificity routing — detect the *type* of task in the request
//! (coding, web_browsing, data_analysis, image_generation, ...)
//! and route to a per-category primary model instead of the tier's
//! default primary.
//!
//! Inspired by `mnfst/manifest`'s specificity system: 9 task-type
//! categories, weighted keyword matching, tool-name prefix signals,
//! header override, and a small session stickiness bias so a 2-hour
//! coding session doesn't oscillate to a different category on every
//! message.
//!
//! ## Resolution order
//!
//! 1. `X-Router-Specificity` header (explicit override)
//! 2. Tool-name prefix match (strong signal — `browser_*` → web_browsing)
//! 3. Weighted keyword scoring across the last user message
//! 4. Session stickiness bias (last N requests same category)
//! 5. If a category clears its activation threshold AND has a
//!    configured primary in `[specificity.<category>]`, route to it.
//! 6. Otherwise, fall through to the tier scorer.

use super::super::config::types::{
    RouterConfig, SpecificityCategory, SpecificityConfig, SpecificityRule,
};
use super::super::schema::inbound::InboundRequest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::collections::HashSet;

/// Default activation threshold per category. A category must score
/// at or above this to be selected. Tuned conservatively — better to
/// fall through to tier routing than to mis-route.
fn default_threshold(category: SpecificityCategory) -> u32 {
    match category {
        SpecificityCategory::Coding => 2,
        SpecificityCategory::WebBrowsing => 3,
        SpecificityCategory::DataAnalysis => 3,
        SpecificityCategory::ImageGeneration => 4,
        SpecificityCategory::VideoGeneration => 4,
        SpecificityCategory::SocialMedia => 3,
        SpecificityCategory::EmailManagement => 3,
        SpecificityCategory::CalendarManagement => 3,
        SpecificityCategory::Trading => 3,
    }
}

/// Per-category keyword dictionary. Lowercased on both sides; word
/// boundaries are NOT enforced (we want substring matches so e.g.
/// `"encrypting"` lights up `coding`). Each (category, keyword) pair
/// carries a weight (default 1) for future tuning.
fn default_keywords(category: SpecificityCategory) -> &'static [(&'static str, u32)] {
    match category {
        SpecificityCategory::Coding => &[
            ("code", 1),
            ("function", 1),
            ("bug", 1),
            ("compile", 2),
            ("debug", 2),
            ("refactor", 2),
            ("syntax", 1),
            ("import", 1),
            ("class", 1),
            ("method", 1),
            ("variable", 1),
            ("library", 1),
            ("api", 1),
            ("endpoint", 1),
            ("compiler", 2),
            ("runtime", 1),
            ("thread", 1),
            ("async", 1),
            ("promise", 1),
            ("kernel", 1),
            ("typescript", 2),
            ("python", 1),
            ("javascript", 1),
            ("rust", 2),
            ("golang", 2),
            ("java", 1),
            ("kotlin", 1),
            ("swift", 1),
            ("html", 1),
            ("css", 1),
            ("react", 1),
            ("vue", 1),
            ("angular", 1),
            ("git", 1),
            ("commit", 1),
            ("merge", 1),
            ("rebase", 1),
            ("stack trace", 2),
            ("stacktrace", 2),
            ("exception", 1),
            ("stack overflow", 2),
            ("lint", 1),
            ("eslint", 2),
            ("prettier", 2),
        ],
        SpecificityCategory::WebBrowsing => &[
            ("http", 1),
            ("https", 1),
            ("url", 1),
            ("website", 1),
            ("webpage", 1),
            ("browse", 2),
            ("navigate", 1),
            ("click", 1),
            ("crawl", 2),
            ("scrape", 2),
            ("fetch", 1),
            ("download", 1),
            ("bookmark", 2),
            ("browser", 2),
            ("chrome", 1),
            ("firefox", 1),
            ("safari", 1),
            ("page", 1),
            ("link", 1),
            ("anchor", 1),
            ("form", 1),
            ("input field", 1),
            ("cookie", 1),
            ("session", 1),
        ],
        SpecificityCategory::DataAnalysis => &[
            ("dataset", 2),
            ("dataframe", 2),
            ("sql", 1),
            ("query", 1),
            ("select", 1),
            ("where", 1),
            ("join", 1),
            ("group by", 2),
            ("aggregate", 2),
            ("sum", 1),
            ("average", 1),
            ("mean", 1),
            ("median", 1),
            ("histogram", 2),
            ("scatter", 2),
            ("regression", 2),
            ("correlation", 2),
            ("p-value", 2),
            ("statistics", 1),
            ("pandas", 2),
            ("numpy", 2),
            ("matplotlib", 2),
            ("csv", 1),
            ("excel", 1),
            ("table", 1),
            ("pivot", 1),
            ("chart", 1),
            ("graph", 1),
            ("plot", 1),
            ("visualize", 2),
            ("dashboard", 1),
            ("kpi", 2),
            ("metric", 1),
            ("benchmark", 1),
        ],
        SpecificityCategory::ImageGeneration => &[
            ("image", 1),
            ("picture", 1),
            ("photo", 1),
            ("illustration", 2),
            ("draw", 2),
            ("paint", 2),
            ("render", 1),
            ("sketch", 2),
            ("midjourney", 3),
            ("dall-e", 3),
            ("stable diffusion", 3),
            ("sdxl", 3),
            ("firefly", 3),
            ("leonardo", 3),
            ("imagen", 3),
            ("flux", 2),
            ("comfyui", 3),
            ("automatic1111", 3),
            ("a1111", 2),
            ("prompt", 1),
            ("negative prompt", 3),
            ("seed", 1),
            ("guidance", 1),
            ("denoise", 2),
            ("upscale", 2),
            ("inpaint", 2),
            ("outpaint", 2),
            ("diffusion", 2),
            ("checkpoint", 1),
            ("lora", 2),
        ],
        SpecificityCategory::VideoGeneration => &[
            ("video", 1),
            ("clip", 1),
            ("movie", 1),
            ("animation", 2),
            ("animate", 2),
            ("sora", 3),
            ("runway", 3),
            ("kling", 3),
            ("pika", 3),
            ("veo", 3),
            ("luma", 3),
            ("hailuo", 3),
            ("frame", 1),
            ("fps", 1),
            ("scene", 1),
            ("shot", 1),
            ("storyboard", 2),
            ("film", 1),
            ("render", 1),
            ("motion", 2),
            ("temporal", 1),
            ("keyframe", 2),
            ("interpolation", 2),
            ("diffusion", 1),
            ("video diffusion", 3),
        ],
        SpecificityCategory::SocialMedia => &[
            ("tweet", 2),
            ("post", 1),
            ("thread", 1),
            ("retweet", 2),
            ("like", 1),
            ("share", 1),
            ("follower", 1),
            ("engagement", 2),
            ("hashtag", 2),
            ("mention", 1),
            ("dm", 2),
            ("instagram", 2),
            ("facebook", 2),
            ("twitter", 2),
            ("x.com", 2),
            ("linkedin", 2),
            ("tiktok", 2),
            ("youtube", 1),
            ("reddit", 2),
            ("mastodon", 2),
            ("bluesky", 2),
            ("hootsuite", 3),
            ("buffer", 2),
            ("impressions", 2),
            ("reach", 1),
            ("virality", 2),
            ("influencer", 2),
            ("trending", 2),
        ],
        SpecificityCategory::EmailManagement => &[
            ("email", 1),
            ("inbox", 2),
            ("outbox", 2),
            ("draft", 1),
            ("reply", 1),
            ("forward", 2),
            ("cc", 1),
            ("bcc", 1),
            ("subject", 1),
            ("attachment", 2),
            ("signature", 1),
            ("gmail", 3),
            ("outlook", 3),
            ("superhuman", 3),
            ("smtp", 2),
            ("imap", 2),
            ("pop3", 2),
            ("mailgun", 3),
            ("sendgrid", 3),
            ("newsletter", 2),
            ("unsubscribe", 2),
            ("spam", 1),
            ("phishing", 2),
            ("mail merge", 3),
        ],
        SpecificityCategory::CalendarManagement => &[
            ("calendar", 2),
            ("meeting", 1),
            ("appointment", 2),
            ("schedule", 1),
            ("reschedule", 2),
            ("invite", 1),
            ("attendee", 2),
            ("agenda", 2),
            ("reminder", 1),
            ("google calendar", 3),
            ("gcal", 3),
            ("calendly", 3),
            ("reclaim", 3),
            ("cal.com", 3),
            ("ics", 2),
            ("ical", 2),
            ("timezone", 1),
            ("recurring", 2),
            ("slot", 1),
            ("availability", 2),
            ("booking", 2),
            ("rsvp", 2),
            ("event", 1),
            ("venue", 1),
        ],
        SpecificityCategory::Trading => &[
            ("trade", 2),
            ("stock", 1),
            ("equity", 1),
            ("option", 1),
            ("future", 1),
            ("bond", 1),
            ("etf", 2),
            ("crypto", 2),
            ("bitcoin", 2),
            ("ethereum", 2),
            ("btc", 2),
            ("eth", 2),
            ("buy", 1),
            ("sell", 2),
            ("short", 1),
            ("long", 1),
            ("limit order", 3),
            ("market order", 3),
            ("stop loss", 3),
            ("take profit", 3),
            ("portfolio", 2),
            ("position", 1),
            ("hedge", 2),
            ("leverage", 2),
            ("margin", 1),
            ("dividend", 2),
            ("earnings", 2),
            ("p/e", 2),
            ("rsi", 2),
            ("macd", 2),
            ("moving average", 3),
            ("bollinger", 3),
            ("candlestick", 3),
            ("robinhood", 3),
            ("coinbase", 3),
            ("binance", 3),
            ("kalshi", 3),
            ("polymarket", 3),
            ("alpaca", 3),
        ],
    }
}

/// Tool-name prefix → category. A single match boosts the category
/// by `TOOL_MATCH_WEIGHT` (3) — same as a strong keyword anchor.
fn tool_prefix_to_category() -> &'static [(&'static str, SpecificityCategory)] {
    &[
        ("browser_", SpecificityCategory::WebBrowsing),
        ("playwright_", SpecificityCategory::WebBrowsing),
        ("web_", SpecificityCategory::WebBrowsing),
        ("scrape_", SpecificityCategory::WebBrowsing),
        ("fetch_", SpecificityCategory::WebBrowsing),
        ("code_", SpecificityCategory::Coding),
        ("editor_", SpecificityCategory::Coding),
        ("lint_", SpecificityCategory::Coding),
        ("git_", SpecificityCategory::Coding),
        ("image_", SpecificityCategory::ImageGeneration),
        ("midjourney_", SpecificityCategory::ImageGeneration),
        ("firefly_", SpecificityCategory::ImageGeneration),
        ("leonardo_", SpecificityCategory::ImageGeneration),
        ("dalle_", SpecificityCategory::ImageGeneration),
        ("imagen_", SpecificityCategory::ImageGeneration),
        ("sora_", SpecificityCategory::VideoGeneration),
        ("runway_", SpecificityCategory::VideoGeneration),
        ("kling_", SpecificityCategory::VideoGeneration),
        ("pika_", SpecificityCategory::VideoGeneration),
        ("veo_", SpecificityCategory::VideoGeneration),
        ("video_", SpecificityCategory::VideoGeneration),
        ("social_", SpecificityCategory::SocialMedia),
        ("hootsuite_", SpecificityCategory::SocialMedia),
        ("buffer_", SpecificityCategory::SocialMedia),
        ("twitter_", SpecificityCategory::SocialMedia),
        ("email_", SpecificityCategory::EmailManagement),
        ("gmail_", SpecificityCategory::EmailManagement),
        ("outlook_", SpecificityCategory::EmailManagement),
        ("smtp_", SpecificityCategory::EmailManagement),
        ("imap_", SpecificityCategory::EmailManagement),
        ("superhuman_", SpecificityCategory::EmailManagement),
        ("calendar_", SpecificityCategory::CalendarManagement),
        ("gcal_", SpecificityCategory::CalendarManagement),
        ("calendly_", SpecificityCategory::CalendarManagement),
        ("reclaim_", SpecificityCategory::CalendarManagement),
        ("ics_", SpecificityCategory::CalendarManagement),
        ("trade_", SpecificityCategory::Trading),
        ("exchange_", SpecificityCategory::Trading),
        ("robinhood_", SpecificityCategory::Trading),
        ("kalshi_", SpecificityCategory::Trading),
        ("coinbase_", SpecificityCategory::Trading),
        ("binance_", SpecificityCategory::Trading),
        ("alpaca_", SpecificityCategory::Trading),
    ]
}

const TOOL_MATCH_WEIGHT: u32 = 3;
const STICKY_HISTORY_WINDOW: usize = 3;
const STICKY_AGREEMENT_MIN: usize = 3;
const STICKY_BIAS: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecificityDecision {
    pub category: SpecificityCategory,
    pub score: u32,
    pub threshold: u32,
    /// The configured primary for this category, if one exists.
    /// `None` means we detected the category but the user hasn't
    /// routed it — caller falls back to tier routing.
    pub primary: Option<String>,
    /// Free-text reasoning for the response header / debug log.
    pub reason: String,
}

/// Detector. Holds the active config snapshot — the scorer clones it
/// into the request context.
#[derive(Clone)]
pub struct SpecificityDetector {
    config: SpecificityConfig,
}

impl SpecificityDetector {
    pub fn new(config: SpecificityConfig) -> Self {
        Self { config }
    }

    /// Run the detector. Returns `Some(decision)` if a category
    /// activates AND has a configured primary. Returns `None` to
    /// signal "fall back to tier routing".
    pub fn detect(
        &self,
        inbound: &InboundRequest,
        header_override: Option<&str>,
        recent: &[SpecificityCategory],
    ) -> Option<SpecificityDecision> {
        if !self.config.enabled {
            return None;
        }

        // 1. Header override (always wins, no scoring needed)
        if let Some(h) = header_override {
            if let Some(cat) = SpecificityCategory::parse(h) {
                if let Some(primary) = self.config.primary_for(cat) {
                    return Some(SpecificityDecision {
                        category: cat,
                        score: u32::MAX,
                        threshold: 0,
                        primary: Some(primary.to_string()),
                        reason: format!("header override (x-router-specificity={})", h),
                    });
                }
            }
        }

        // 2 + 3. Score every category on (a) tool-name prefixes,
        // (b) weighted keyword matches in the user text.
        let mut scores: HashMap<SpecificityCategory, u32> = SpecificityCategory::all()
            .iter()
            .map(|c| (*c, 0u32))
            .collect();

        // Tool-name prefix matches
        if let Some(tools) = &inbound.tools {
            for tool in tools {
                let name = tool.function.name.to_lowercase();
                for (prefix, cat) in tool_prefix_to_category() {
                    if name.starts_with(prefix) {
                        *scores.entry(*cat).or_insert(0) += TOOL_MATCH_WEIGHT;
                    }
                }
            }
        }

        // User-text keyword scoring (last user message only — older
        // history is too noisy and dilutes the signal)
        let last_user_text = last_user_text(inbound).to_lowercase();
        for cat in SpecificityCategory::all() {
            let kws = default_keywords(*cat);
            for (kw, weight) in kws {
                if last_user_text.contains(kw) {
                    *scores.entry(*cat).or_insert(0) += weight;
                }
            }
        }

        // 4. Session stickiness — if the last STICKY_HISTORY_WINDOW
        // requests all classified to the same category, give that
        // category a small bias so an ambiguous current message
        // doesn't flip.
        if recent.len() >= STICKY_AGREEMENT_MIN {
            let last = &recent[recent.len() - STICKY_HISTORY_WINDOW..];
            let unique: HashSet<_> = last.iter().copied().collect();
            if unique.len() == 1 {
                if let Some(cat) = unique.iter().next() {
                    *scores.entry(*cat).or_insert(0) += STICKY_BIAS;
                }
            }
        }

        // 5. Pick the highest-scoring category that clears its
        // threshold AND has a configured primary.
        let mut best: Option<(SpecificityCategory, u32, u32)> = None;
        for (cat, score) in &scores {
            let threshold = self
                .config
                .threshold_for(*cat)
                .unwrap_or_else(|| default_threshold(*cat));
            if *score < threshold {
                continue;
            }
            if self.config.primary_for(*cat).is_none() {
                continue;
            }
            match best {
                Some((_, bs, bt)) if *score < bs => {}
                Some((_, bs, bt)) if *score == bs && threshold <= bt => {}
                _ => best = Some((*cat, *score, threshold)),
            }
        }

        let (cat, score, threshold) = best?;
        let primary = self.config.primary_for(cat)?;
        Some(SpecificityDecision {
            category: cat,
            score,
            threshold,
            primary: Some(primary.to_string()),
            reason: format!("detected {cat} (score={score}, threshold={threshold})"),
        })
    }

    /// Total weight of configured keyword buckets across all categories.
    pub fn keyword_count(&self) -> usize {
        SpecificityCategory::all()
            .iter()
            .map(|c| default_keywords(*c).len())
            .sum()
    }
}

/// Extract the last user-role message text from a request. Returns
/// empty string if there are no user messages. Inline strings and
/// array-of-content-parts are both supported.
fn last_user_text(req: &InboundRequest) -> String {
    let last = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .unwrap_or(&req.messages[0]);
    extract_text(&last.content)
}

fn extract_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Helper: build the detector from a RouterConfig snapshot. The
/// scorer uses this to keep its async surface narrow.
pub fn detector_from_config(config: &RouterConfig) -> SpecificityDetector {
    SpecificityDetector::new(config.specificity.clone())
}

/// Re-export so callers don't have to chase imports.
pub use super::super::config::types::SpecificityCategory as Category;

/// Inline extension trait — keeps the detector's call sites short
/// (`self.config.primary_for(cat)`) instead of `self.config.rules
/// .iter().find(...)` everywhere.
trait SpecificityConfigExt {
    fn threshold_for(&self, c: SpecificityCategory) -> Option<u32>;
    fn primary_for(&self, c: SpecificityCategory) -> Option<&str>;
}
impl SpecificityConfigExt for SpecificityConfig {
    fn threshold_for(&self, c: SpecificityCategory) -> Option<u32> {
        self.rules
            .iter()
            .find(|r| r.category == c)
            .and_then(|r| r.threshold)
    }
    fn primary_for(&self, c: SpecificityCategory) -> Option<&str> {
        self.rules
            .iter()
            .find(|r| r.category == c)
            .map(|r| r.primary.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::inbound::{InboundMessage, InboundRequest, InboundTool, InboundToolDef};

    fn make_req(text: &str) -> InboundRequest {
        InboundRequest {
            model: "auto".to_string(),
            messages: vec![InboundMessage {
                role: "user".to_string(),
                content: serde_json::Value::String(text.to_string()),
                name: None,
                tool_call_id: None,
                tool_calls: None,
            }],
            max_tokens: None,
            temperature: None,
            top_p: None,
            stop: None,
            stream: false,
            tools: None,
            tool_choice: None,
        }
    }

    fn make_req_with_tools(text: &str, tool_names: &[&str]) -> InboundRequest {
        let mut req = make_req(text);
        req.tools = Some(
            tool_names
                .iter()
                .map(|n| InboundTool {
                    kind: "function".to_string(),
                    function: InboundToolDef {
                        name: n.to_string(),
                        description: None,
                        parameters: serde_json::json!({}),
                    },
                })
                .collect(),
        );
        req
    }

    fn detector_with(cat: SpecificityCategory, primary: &str) -> SpecificityDetector {
        SpecificityDetector::new(SpecificityConfig {
            enabled: true,
            rules: vec![SpecificityRule {
                category: cat,
                primary: primary.to_string(),
                threshold: None,
            }],
        })
    }

    #[test]
    fn coding_keywords_route_to_coding_primary() {
        let det = detector_with(SpecificityCategory::Coding, "anthropic/claude-sonnet-4-5");
        let req = make_req("Please refactor this function to use async/await syntax");
        let decision = det.detect(&req, None, &[]).expect("decision");
        assert_eq!(decision.category, SpecificityCategory::Coding);
        assert_eq!(
            decision.primary.as_deref(),
            Some("anthropic/claude-sonnet-4-5")
        );
    }

    #[test]
    fn tool_name_prefix_overrides_keywords() {
        let det = detector_with(SpecificityCategory::WebBrowsing, "browser/model");
        let det2 = detector_with(SpecificityCategory::ImageGeneration, "img/model");
        let req = make_req_with_tools("do something", &["browser_navigate", "browser_click"]);
        // Tool prefix is a strong signal — should activate even on
        // an empty user message.
        let d1 = det.detect(&req, None, &[]).expect("decision");
        assert_eq!(d1.category, SpecificityCategory::WebBrowsing);

        // Different tool prefix → different category
        let req2 = make_req_with_tools("draw a cat", &["midjourney_generate"]);
        let d2 = det2.detect(&req2, None, &[]).expect("decision");
        assert_eq!(d2.category, SpecificityCategory::ImageGeneration);
    }

    #[test]
    fn header_override_always_wins() {
        let det = detector_with(SpecificityCategory::Trading, "trader/model");
        let req = make_req("tell me a story");
        let d = det.detect(&req, Some("trading"), &[]).expect("decision");
        assert_eq!(d.category, SpecificityCategory::Trading);
        assert!(d.reason.contains("header override"));
    }

    #[test]
    fn below_threshold_falls_through() {
        let det = detector_with(SpecificityCategory::Trading, "trader/model");
        // "stock" alone is one keyword weight — below threshold of 3.
        let req = make_req("check the stock of coffee in the kitchen");
        assert!(det.detect(&req, None, &[]).is_none());
    }

    #[test]
    fn specificity_disabled_returns_none() {
        let det = SpecificityDetector::new(SpecificityConfig {
            enabled: false,
            rules: vec![SpecificityRule {
                category: SpecificityCategory::Coding,
                primary: "x/y".to_string(),
                threshold: None,
            }],
        });
        let req = make_req("refactor this code function");
        assert!(det.detect(&req, None, &[]).is_none());
    }

    #[test]
    fn unconfigured_category_falls_through() {
        // No rule for `email_management`, even though the text would
        // match keywords — caller falls back to tier routing.
        let det = detector_with(SpecificityCategory::Coding, "x/y");
        let req = make_req("send an email via gmail with this attachment");
        assert!(det.detect(&req, None, &[]).is_none());
    }

    #[test]
    fn sticky_bias_keeps_coding_session() {
        let det = detector_with(SpecificityCategory::Coding, "coder/model");
        // Borderline: only 2 keywords ("function", "code"), under the
        // default threshold of 2 — wait, that equals threshold.
        // Use "function" alone (1 keyword, score 1, below threshold).
        let req = make_req("fix this function");
        // Without sticky history, no decision.
        assert!(det.detect(&req, None, &[]).is_none());
        // With a sticky history of 3 codings, the bias pushes us over.
        let history = vec![
            SpecificityCategory::Coding,
            SpecificityCategory::Coding,
            SpecificityCategory::Coding,
        ];
        let d = det.detect(&req, None, &history).expect("decision");
        assert_eq!(d.category, SpecificityCategory::Coding);
    }

    #[test]
    fn data_analysis_routes_correctly() {
        let det = detector_with(
            SpecificityCategory::DataAnalysis,
            "anthropic/claude-opus-4-5",
        );
        let req = make_req(
            "Run a SQL query with group by and aggregate to build a histogram of the dataset's mean",
        );
        let d = det.detect(&req, None, &[]).expect("decision");
        assert_eq!(d.category, SpecificityCategory::DataAnalysis);
    }
}
