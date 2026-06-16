//! token-dealer — high-performance LLM routing proxy.
//! Public surface for integration tests and embedders.

pub mod config;
pub mod cost;
pub mod db;
pub mod error;
pub mod log;
pub mod metadata;
pub mod providers;
pub mod proxy;
pub mod routing;
pub mod schema;
pub mod server;

use std::sync::Arc;

/// Top-level application state. Cloned cheaply (Arc-shared internals).
#[derive(Clone)]
pub struct AppState {
    pub pipeline: Arc<proxy::pipeline::Pipeline>,
    pub config: config::ConfigService,
    pub health: providers::HealthRegistry,
    pub db: db::Db,
    pub metadata: metadata::MetadataStore,
}

impl AppState {
    pub fn new(
        pipeline: proxy::pipeline::Pipeline,
        config: config::ConfigService,
        health: providers::HealthRegistry,
        db: db::Db,
        metadata: metadata::MetadataStore,
    ) -> Self {
        Self {
            pipeline: Arc::new(pipeline),
            config,
            health,
            db,
            metadata,
        }
    }
}
