//! Axum Router. Wires the middleware stack and the handlers.

use super::auth;
use super::handlers::{chat_completions, health, healthz, list_models, public_stats, reload_config};
use super::middleware::request_id_layer;
use super::multimodal::{audio_speech, image_generations, video_generations};
use super::ui::{
    dashboard, index, logs_page, playground_page, playground_send, providers_new_step1,
    providers_new_step2, providers_page, providers_partial, rules_page, tiers_page,
    ui_remove_provider, ui_style,
};
use super::AppState;
use super::admin::{
    add_provider, add_rule, delete_key, delete_rule, list_provider_models,
    list_provider_types, oauth_callback, poll_device_oauth, remove_provider, save_config,
    set_key, set_oauth_refresh, start_device_oauth, start_oauth, test_provider, update_tier,
    validate_provider_type,
};
use super::auth_endpoints;
use super::ui_login as login_pages;
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
        .route("/admin/providers/list-models", post(list_provider_models))
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
        .route("/admin/oauth/:provider_id/refresh", post(set_oauth_refresh))
        .route("/admin/oauth/:provider_id/start", post(start_oauth))
        .route("/admin/oauth/:provider_id/callback", get(oauth_callback))
        .route("/admin/oauth/:provider_id/device/start", post(start_device_oauth))
        .route("/admin/oauth/device/poll", post(poll_device_oauth))
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
        .route("/ui/playground", get(playground_page).post(playground_send))
        .route("/ui/users", get(super::ui::users_page))
        .route("/ui/account", get(super::ui::account_page))
        .route("/ui/pricing", get(super::ui::pricing_page))
        .route("/ui/style.css", get(ui_style))
        .route("/ui/login", get(login_pages::login_page))
        .route("/ui/setup", get(login_pages::setup_page).post(login_pages::setup_submit))
        .route("/admin/ui/remove/:id", post(ui_remove_provider))
        // Auth (login + me + usage + own keys)
        .route("/auth/login", post(auth_endpoints::login))
        .route("/auth/logout", post(auth_endpoints::logout))
        .route("/auth/me", get(auth_endpoints::me))
        .route("/auth/keys", get(auth_endpoints::list_own_keys).post(auth_endpoints::create_own_key))
        .route("/auth/keys/:id", axum::routing::delete(auth_endpoints::delete_own_key))
        .route("/auth/usage", get(auth_endpoints::my_usage))
        // Admin user management
        .route("/admin/users", get(auth_endpoints::list_users).post(auth_endpoints::create_user))
        .route("/admin/users/:id", axum::routing::delete(auth_endpoints::delete_user))
        .route("/admin/users/:id/keys", post(auth_endpoints::admin_create_key))
        // Public stats for the marketing site
        .route("/v1/stats", get(public_stats))
        .route("/healthz", get(healthz))
        .with_state(state.clone())
        .layer(from_fn_with_state(state.clone(), auth::middleware))
        .layer(TraceLayer::new_for_http())
        .layer(request_id_layer())
}

