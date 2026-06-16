//! token-dealer — high-performance LLM routing proxy.
//! Public surface for integration tests and embedders.

pub mod config;
pub mod error;
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
}

impl AppState {
    pub fn new(
        pipeline: proxy::pipeline::Pipeline,
        config: config::ConfigService,
        health: providers::HealthRegistry,
    ) -> Self {
        Self {
            pipeline: Arc::new(pipeline),
            config,
            health,
        }
    }
}
