//! token-dealer — high-performance LLM routing proxy.
//! Public surface for integration tests and embedders.

pub mod agents;
pub mod auth;
pub mod config;
pub mod cost;
pub mod db;
pub mod error;
pub mod log;
pub mod metadata;
pub mod oauth;
pub mod providers;
pub mod proxy;
pub mod routing;
pub mod schema;
pub mod server;
pub mod telemetry;
pub mod tokens;

use std::sync::Arc;

/// Top-level application state. Cloned cheaply (Arc-shared internals).
#[derive(Clone)]
pub struct AppState {
    pub pipeline: Arc<proxy::pipeline::Pipeline>,
    pub config: config::ConfigService,
    pub health: providers::HealthRegistry,
    pub db: db::Db,
    pub metadata: metadata::MetadataStore,
    pub key_store: auth::KeyStore,
    pub oauth: oauth::OAuthManager,
    pub user_store: auth::UserStore,
    pub pricing: cost::PricingStore,
    pub telemetry: telemetry::Telemetry,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pipeline: proxy::pipeline::Pipeline,
        config: config::ConfigService,
        health: providers::HealthRegistry,
        db: db::Db,
        metadata: metadata::MetadataStore,
        key_store: auth::KeyStore,
        oauth: oauth::OAuthManager,
        user_store: auth::UserStore,
        pricing: cost::PricingStore,
        telemetry: telemetry::Telemetry,
    ) -> Self {
        Self {
            pipeline: Arc::new(pipeline),
            config,
            health,
            db,
            metadata,
            key_store,
            oauth,
            user_store,
            pricing,
            telemetry,
        }
    }
}
