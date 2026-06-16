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
}

fn default_log_level() -> String {
    "info".into()
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:8080".into(),
            log_level: "info".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub enabled: bool,
    #[serde(default)]
    pub keys: Vec<AuthKey>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
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
#[serde(rename_all = "lowercase")]
pub enum ProviderType {
    Anthropic,
    Openai,
    Google,
    Generic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    #[serde(rename = "type", default = "default_provider_type")]
    pub provider_type: ProviderType,
    #[serde(default)]
    pub key: Option<String>,
    pub base_url: String,
    #[serde(default)]
    pub default_model: Option<String>,
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
    #[serde(default)]
    pub detection: DetectionConfig,
    #[serde(default)]
    pub retry: RetryConfig,
    #[serde(default)]
    pub streaming: StreamingConfig,
}

impl RouterConfig {
    /// Resolve the primary model for a tier. Returns the canonical
    /// `provider/model` string.
    pub fn primary_for_tier(&self, tier: Tier) -> Option<&str> {
        self.tiers.get(tier.as_str()).map(|t| t.primary.as_str())
    }
}
