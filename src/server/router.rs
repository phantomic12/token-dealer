//! Axum Router. Wires the middleware stack and the handlers.

use super::handlers::{chat_completions, health, list_models, reload_config};
use super::middleware::request_id_layer;
use super::AppState;
use axum::{
    routing::{get, post},
    Router,
};
use tower_http::trace::TraceLayer;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(list_models))
        .route("/v1/health", get(health))
        .route("/health", get(health))
        .route("/admin/config/reload", post(reload_config))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
        .layer(request_id_layer())
}
