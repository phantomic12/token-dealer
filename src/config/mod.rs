pub mod service;
pub mod types;
pub mod validate;

pub use service::ConfigService;
pub use types::{
    AuthConfig, AuthKey, BudgetConfig, DatabaseConfig, DetectionCondition, DetectionConfig,
    DetectionRule, DiscoveryConfig, PricingSyncConfig, ProviderConfig, ProviderType,
    RateLimitBucket, RateLimitConfig, RetryConfig, RouterConfig, ServerConfig, SpecificityCategory,
    SpecificityConfig, SpecificityRule, StreamingConfig, TierConfig, TierTimeouts, TierTimeoutsSet,
};
pub use validate::{validate as validate_config, ConfigError, ConfigWarning, ValidationOutcome};

use crate::auth::resolve as resolve_key;
