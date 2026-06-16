//! Axum Router. Wires the middleware stack and the handlers.

use super::auth;
use super::handlers::{chat_completions, health, list_models, reload_config};
use super::middleware::request_id_layer;
use super::multimodal::{audio_speech, image_generations, video_generations};
use super::ui::{
    dashboard, index, logs_page, providers_new_step1, providers_new_step2, providers_page,
    providers_partial, rules_page, tiers_page, ui_remove_provider, ui_style,
};
use super::AppState;
use super::admin::{
    add_provider, add_rule, delete_key, delete_rule, list_provider_types, remove_provider,
    save_config, set_key, test_provider, update_tier, validate_provider_type,
};
use axum::{
    middleware::from_fn_with_state,
    routing::{get, post},
    Router,
};
use tower_http::trace::TraceLayer;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        // Public API
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(list_models))
        .route("/v1/images/generations", post(image_generations))
        .route("/v1/audio/speech", post(audio_speech))
        .route("/v1/videos/generations", post(video_generations))
        .route("/v1/health", get(health))
        .route("/health", get(health))
        // Admin API (JSON)
        .route("/admin/config/reload", post(reload_config))
        .route("/admin/config/save", post(save_config))
        .route("/admin/providers", post(add_provider).get(list_provider_types))
        .route("/admin/providers/test", post(test_provider))
        .route(
            "/admin/providers/:id",
            post(remove_provider)
                .delete(remove_provider)
                .patch(remove_provider),
        )
        .route(
            "/admin/tiers/:tier",
            post(update_tier).patch(update_tier).put(update_tier),
        )
        .route(
            "/admin/provider-types/validate",
            post(validate_provider_type),
        )
        .route("/admin/rules", post(add_rule))
        .route("/admin/rules/:index", post(delete_rule).delete(delete_rule))
        .route("/admin/keys/:provider_id", post(set_key).delete(delete_key))
        // WebUI
        .route("/", get(index))
        .route("/ui", get(index))
        .route("/ui/", get(dashboard))
        .route("/ui/providers", get(providers_page))
        .route("/ui/providers/new", get(providers_new_step1))
        .route("/ui/providers/new/config", get(providers_new_step2))
        .route("/ui/partials/providers", get(providers_partial))
        .route("/ui/tiers", get(tiers_page))
        .route("/ui/logs", get(logs_page))
        .route("/ui/rules", get(rules_page))
        .route("/ui/style.css", get(ui_style))
        .route("/admin/ui/remove/:id", post(ui_remove_provider))
        .with_state(state.clone())
        .layer(from_fn_with_state(state.clone(), auth::middleware))
        .layer(TraceLayer::new_for_http())
        .layer(request_id_layer())
}

