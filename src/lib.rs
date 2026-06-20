//! token-dealer — high-performance LLM routing proxy.
//! Public surface for integration tests and embedders.

pub mod agents;
pub mod auth;
pub mod config;
pub mod cost;
pub mod db;
pub mod discovery;
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
    /// Master key used to decrypt `enc:`-prefixed values in
    /// config and to derive per-purpose subkeys. Required by
    /// v0.2.0 when `[auth] enabled = true`.
    pub master: auth::MasterKey,
    pub oauth: oauth::OAuthManager,
    pub user_store: auth::UserStore,
    pub pricing: cost::PricingStore,
    pub telemetry: telemetry::Telemetry,
    /// Server-Sent Events broadcast bus. Lazily initialized by the
    /// SSE handler; cheap to clone (broadcast::Sender).
    pub events: Arc<server::events::EventBus>,
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
        master: auth::MasterKey,
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
            master,
            oauth,
            user_store,
            pricing,
            telemetry,
            events: Arc::new(server::events::EventBus::default()),
        }
    }
}
