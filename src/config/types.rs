//! Strongly-typed config structs. TOML deserialization. Defaultable
//! via `Default` impls; the `ConfigService` fills in missing fields.

use super::super::schema::canonical::Tier;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub bind: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// OAuth2 redirect URI used by popup_oauth flows. Must be
    /// publicly reachable from the user's browser.
    #[serde(default)]
    pub oauth_redirect_uri: String,
}

fn default_log_level() -> String {
    "info".into()
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:8080".into(),
            log_level: "info".into(),
            oauth_redirect_uri: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub enabled: bool,
    /// Legacy single admin key. New code uses the `users` table
    /// + API keys. This is here so existing `token-dealer.toml`
    /// configs without a user table still work.
    #[serde(default)]
    pub admin_key: Option<String>,
    #[serde(default)]
    pub keys: Vec<AuthKey>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            admin_key: None,
            keys: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthKey {
    pub key: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderType {
    // Wire formats with their own adapters
    Anthropic,
    Google,
    Kiro,
    Responses,
    Generic,

    // OpenAI-compatible providers (all use OpenAiAdapter with provider-specific base_url/path)
    Openai,
    Openrouter,
    Tokenrouter,
    Groq,
    Deepseek,
    Fireworks,
    Mistral,
    Xai,
    Qwen,
    Moonshot,
    Zai,
    Xiaomi,
    Minimax,
    Byteplus,
    Nvidia,
    OpencodeGo,
    OpencodeZen,
    Kilo,
    Commandcode,
    GithubCopilot,
    Gitlawb,
    Ollama,
    OllamaCloud,
    LlamaCpp,
    LmStudio,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    #[serde(rename = "type", default = "default_provider_type")]
    pub provider_type: ProviderType,
    #[serde(default)]
    pub key: Option<String>,
    /// Optional. Defaults to the manifest-known base URL for `type`.
    /// Override here for self-hosted proxies, local-only deployments, or
    /// staging environments.
    pub base_url: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    /// Optional. Default `/v1/chat/completions` for OpenAI-compat. Override
    /// for providers that use a non-standard path (Kilo `/chat/completions`,
    /// BytePlus `/v3/chat/completions`, etc.).
    pub path: Option<String>,
}

fn default_provider_type() -> ProviderType {
    ProviderType::Generic
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TierTimeouts {
    #[serde(default = "default_connect")]
    pub connect_ms: u64,
    #[serde(default = "default_read")]
    pub read_ms: u64,
    #[serde(default = "default_per_attempt")]
    pub per_attempt_ms: u64,
}

fn default_connect() -> u64 {
    5000
}
fn default_read() -> u64 {
    30000
}
fn default_per_attempt() -> u64 {
    45000
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TierTimeoutsSet {
    pub connect_ms: Option<u64>,
    pub read_ms: Option<u64>,
    pub per_attempt_ms: Option<u64>,
}

impl TierTimeoutsSet {
    pub fn merge_into(&self, base: &mut TierTimeouts) {
        if let Some(v) = self.connect_ms {
            base.connect_ms = v;
        }
        if let Some(v) = self.read_ms {
            base.read_ms = v;
        }
        if let Some(v) = self.per_attempt_ms {
            base.per_attempt_ms = v;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierConfig {
    pub primary: String,
    #[serde(default)]
    pub fallbacks: Vec<String>,
    #[serde(default)]
    pub allow_tier_downgrade: bool,
    #[serde(default)]
    pub downgrade_to: Option<String>,
    #[serde(default)]
    pub min_context_window: Option<u32>,
    #[serde(default)]
    pub timeouts: TierTimeoutsSet,
}

impl TierConfig {
    pub fn timeouts(&self) -> TierTimeouts {
        let mut t = TierTimeouts::default();
        self.timeouts.merge_into(&mut t);
        t
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DetectionConfig {
    pub default_tier: Option<String>,
    pub session_window_minutes: Option<u32>,
    pub session_lookback: Option<u32>,
    #[serde(default)]
    pub rules: Vec<DetectionRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectionRule {
    #[serde(rename = "if")]
    pub condition: DetectionCondition,
    pub tier: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectionCondition {
    pub has_tools: Option<bool>,
    pub input_tokens_gt: Option<u32>,
    pub prompt_contains: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    #[serde(default = "default_max_retries")]
    pub max_same_provider_retries: u32,
    #[serde(default = "default_fixed_wait")]
    pub fixed_retry_wait_ms: u64,
    #[serde(default = "default_max_retry_after")]
    pub max_retry_after_ms: u64,
    #[serde(default = "default_request_budget")]
    pub request_budget_ms: u64,
}

/// Specificity categories. Detected from keywords + tool-name prefixes
/// in the request, used to route to a per-category primary model instead
/// of the tier's default primary. Mirrors `mnfst/manifest`'s 9-category
/// specificity system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecificityCategory {
    Coding,
    WebBrowsing,
    DataAnalysis,
    ImageGeneration,
    VideoGeneration,
    SocialMedia,
    EmailManagement,
    CalendarManagement,
    Trading,
}

impl SpecificityCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            SpecificityCategory::Coding => "coding",
            SpecificityCategory::WebBrowsing => "web_browsing",
            SpecificityCategory::DataAnalysis => "data_analysis",
            SpecificityCategory::ImageGeneration => "image_generation",
            SpecificityCategory::VideoGeneration => "video_generation",
            SpecificityCategory::SocialMedia => "social_media",
            SpecificityCategory::EmailManagement => "email_management",
            SpecificityCategory::CalendarManagement => "calendar_management",
            SpecificityCategory::Trading => "trading",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "coding" => SpecificityCategory::Coding,
            "web_browsing" | "web-browsing" | "browsing" => SpecificityCategory::WebBrowsing,
            "data_analysis" | "data-analysis" | "analysis" => SpecificityCategory::DataAnalysis,
            "image_generation" | "image-generation" | "image" => {
                SpecificityCategory::ImageGeneration
            }
            "video_generation" | "video-generation" | "video" => {
                SpecificityCategory::VideoGeneration
            }
            "social_media" | "social-media" | "social" => SpecificityCategory::SocialMedia,
            "email_management" | "email-management" | "email" => {
                SpecificityCategory::EmailManagement
            }
            "calendar_management" | "calendar-management" | "calendar" => {
                SpecificityCategory::CalendarManagement
            }
            "trading" => SpecificityCategory::Trading,
            _ => return None,
        })
    }
    pub fn all() -> &'static [SpecificityCategory] {
        &[
            SpecificityCategory::Coding,
            SpecificityCategory::WebBrowsing,
            SpecificityCategory::DataAnalysis,
            SpecificityCategory::ImageGeneration,
            SpecificityCategory::VideoGeneration,
            SpecificityCategory::SocialMedia,
            SpecificityCategory::EmailManagement,
            SpecificityCategory::CalendarManagement,
            SpecificityCategory::Trading,
        ]
    }
}

impl std::fmt::Display for SpecificityCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Default for SpecificityCategory {
    fn default() -> Self {
        SpecificityCategory::Coding
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecificityRule {
    pub category: SpecificityCategory,
    /// The model to route to when this category is detected. Same
    /// `provider/model` syntax as tier primaries.
    pub primary: String,
    /// Optional activation threshold override (default per-category
    /// values live in the detector module).
    #[serde(default)]
    pub threshold: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpecificityConfig {
    /// Master switch. When false, the detector is skipped entirely
    /// and the tier scorer owns routing.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub rules: Vec<SpecificityRule>,
}

/// Per-tier + per-day + per-user cost / token budgets. Enforced
/// at request-time by the chat handler before dispatch. Returns
/// 429 + OpenAI-shape error envelope when exceeded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    /// Max USD per calendar day (UTC) across all requests. 0 = unlimited.
    #[serde(default)]
    pub daily_cost_usd: f64,
    /// Max USD per single request. 0 = unlimited.
    #[serde(default)]
    pub per_request_cost_usd: f64,
    /// Soft warning at this fraction of the daily cap (default 0.8).
    #[serde(default = "default_warn_fraction")]
    pub warn_fraction: f64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            daily_cost_usd: 0.0,
            per_request_cost_usd: 0.0,
            warn_fraction: default_warn_fraction(),
        }
    }
}

fn default_warn_fraction() -> f64 {
    0.8
}

/// Pricing sync configuration. OpenRouter provides a free, no-auth
/// JSON catalog of 300+ model prices that token-dealer ingests
/// daily. Manual seeding of `model_prices` is still supported via
/// `POST /admin/pricing`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingSyncConfig {
    /// Master switch. When true, a background task refreshes the
    /// `model_prices` table from OpenRouter at startup and every
    /// `interval_hours`.
    #[serde(default = "default_pricing_sync_enabled")]
    pub enabled: bool,
    #[serde(default = "default_pricing_sync_interval")]
    pub interval_hours: u64,
    /// OpenRouter API base URL. Override only for testing.
    #[serde(default = "default_pricing_sync_url")]
    pub openrouter_url: String,
}

fn default_pricing_sync_enabled() -> bool {
    true
}
fn default_pricing_sync_interval() -> u64 {
    24
}
fn default_pricing_sync_url() -> String {
    "https://openrouter.ai/api/v1/models".to_string()
}

impl Default for PricingSyncConfig {
    fn default() -> Self {
        Self {
            enabled: default_pricing_sync_enabled(),
            interval_hours: default_pricing_sync_interval(),
            openrouter_url: default_pricing_sync_url(),
        }
    }
}

/// Model discovery configuration. On startup (and via admin
/// endpoint), token-dealer fetches `/v1/models` from each connected
/// provider and caches the model list per provider in the
/// `provider_models` table. The `/v1/models` endpoint and tier
/// auto-assignment then use this list instead of relying on a
/// single hard-coded `default_model`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryConfig {
    #[serde(default = "default_discovery_enabled")]
    pub enabled: bool,
    /// If true, on startup auto-assign empty tier primaries from
    /// the discovered model list using the cheapest-with-quality
    /// heuristic. Existing manual primaries are never overwritten.
    #[serde(default)]
    pub auto_assign_tiers: bool,
    /// Models cheaper than this USD/1M input are excluded from
    /// auto-assignment (cheap models tend to be low quality — we
    /// refuse to silently downgrade a user's tier).
    #[serde(default = "default_min_input_price")]
    pub min_input_price_per_1m: f64,
}

fn default_discovery_enabled() -> bool {
    true
}
fn default_min_input_price() -> f64 {
    0.10
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            enabled: default_discovery_enabled(),
            auto_assign_tiers: false,
            min_input_price_per_1m: default_min_input_price(),
        }
    }
}

fn default_max_retries() -> u32 {
    1
}
fn default_fixed_wait() -> u64 {
    1500
}
fn default_max_retry_after() -> u64 {
    10000
}
fn default_request_budget() -> u64 {
    120000
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_same_provider_retries: default_max_retries(),
            fixed_retry_wait_ms: default_fixed_wait(),
            max_retry_after_ms: default_max_retry_after(),
            request_budget_ms: default_request_budget(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamingConfig {
    #[serde(default = "default_buffer_threshold")]
    pub buffer_threshold_tokens: u32,
}

fn default_buffer_threshold() -> u32 {
    2048
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            buffer_threshold_tokens: default_buffer_threshold(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RouterConfig {
    #[serde(default)]
    pub server: ServerConfig,
    /// Legacy alias for `server.oauth_redirect_uri`. Older configs
    /// (and the example shipped with the README) put the redirect
    /// URI at the top level. Accept both shapes; the top-level one
    /// wins when set.
    #[serde(default)]
    pub oauth_redirect_uri: Option<String>,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    /// Tier table. Uses a flat map for forward-compat with hot-reload
    /// and dynamic tier addition.
    #[serde(default)]
    pub tiers: HashMap<String, TierConfig>,
    /// Per-tier key overrides. The key for tier `simple` here takes
    /// precedence over the provider's TOML `key` field, the env var,
    /// and the encrypted store when a request lands on the `simple`
    /// tier. Useful for BYOK billing separation or sandboxing.
    #[serde(default)]
    pub tier_keys: HashMap<String, String>,
    #[serde(default)]
    pub detection: DetectionConfig,
    #[serde(default)]
    pub retry: RetryConfig,
    #[serde(default)]
    pub streaming: StreamingConfig,
    /// Specificity routing. When enabled, the request is classified
    /// into one of 9 task categories (coding, web_browsing, etc.)
    /// and routed to a per-category primary model. Falls through to
    /// tier routing when no category activates.
    #[serde(default)]
    pub specificity: SpecificityConfig,
    /// Cost / token budgets enforced at request-time.
    #[serde(default)]
    pub budgets: BudgetConfig,
    /// OpenRouter pricing sync. Background task that ingests the
    /// 300+ model price catalog daily.
    #[serde(default)]
    pub pricing_sync: PricingSyncConfig,
    /// Model discovery. On startup, fetch `/v1/models` from each
    /// connected provider and cache the model list per provider.
    #[serde(default)]
    pub discovery: DiscoveryConfig,
    /// SQLite log retention in days. 0 = forever (default).
    #[serde(default)]
    pub log_retention_days: u32,
}

impl RouterConfig {
    /// Resolve the primary model for a tier. Returns the canonical
    /// `provider/model` string.
    pub fn primary_for_tier(&self, tier: Tier) -> Option<&str> {
        self.tiers.get(tier.as_str()).map(|t| t.primary.as_str())
    }

    /// Key override for a specific tier, if one is configured.
    /// Returns the raw literal — caller resolves env vars and the
    /// encrypted store.
    pub fn tier_key_override(&self, tier: Tier) -> Option<&str> {
        self.tier_keys.get(tier.as_str()).map(String::as_str)
    }
}
