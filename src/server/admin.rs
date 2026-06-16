//! Admin endpoints. JSON + form-encoded.
//! `POST /admin/providers` adds a provider in-memory and (via the UI's
//! form flow) the change is persisted to TOML through `update_with`.
//! `DELETE /admin/providers/:id` removes a provider.
//! `POST /admin/tiers/:tier` updates a tier's primary + fallbacks + timeouts.
//! `POST /admin/config/save` forces a disk flush.

use super::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;

use crate::config::types::{ProviderConfig, ProviderType, TierConfig};

pub async fn add_provider(
    State(state): State<AppState>,
    Json(body): Json<ProviderConfig>,
) -> Response {
    // Validate type string. serde already does this for typed enums;
    // an unknown variant would have been caught by the JSON parser.
    let _ = body.provider_type;

    let result = state
        .config
        .update_with(|cfg| {
            // Remove any existing entry with the same id, then append.
            cfg.providers.retain(|p| p.id != body.id);
            cfg.providers.push(body.clone());
        })
        .await;

    match result {
        Ok(_) => match state.pipeline.registry.add(&body).await {
            Ok(_) => (StatusCode::CREATED, Json(json!({"status": "added", "id": body.id})))
                .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response(),
        },
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("save failed: {e}")})),
        )
            .into_response(),
    }
}

pub async fn remove_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    if id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "id required"})),
        )
            .into_response();
    }
    let result = state
        .config
        .update_with(|cfg| {
            cfg.providers.retain(|p| p.id != id);
        })
        .await;
    match result {
        Ok(_) => {
            let removed = state.pipeline.registry.remove(&id).await;
            if removed {
                (StatusCode::OK, Json(json!({"status": "removed", "id": id}))).into_response()
            } else {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": format!("provider {id} not found")})),
                )
                    .into_response()
            }
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("save failed: {e}")})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub struct TierUpdate {
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
    pub timeouts: Option<crate::config::types::TierTimeoutsSet>,
}

pub async fn update_tier(
    State(state): State<AppState>,
    Path(tier): Path<String>,
    Json(body): Json<TierUpdate>,
) -> Response {
    if !crate::schema::canonical::Tier::parse(&tier).is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("unknown tier: {tier}")})),
        )
            .into_response();
    }
    let new_tier = TierConfig {
        primary: body.primary,
        fallbacks: body.fallbacks,
        allow_tier_downgrade: body.allow_tier_downgrade,
        downgrade_to: body.downgrade_to,
        min_context_window: body.min_context_window,
        timeouts: body.timeouts.unwrap_or_default(),
    };
    let tier_clone = tier.clone();
    let result = state
        .config
        .update_with(|cfg| {
            cfg.tiers.insert(tier_clone.clone(), new_tier);
        })
        .await;
    match result {
        Ok(_) => (StatusCode::OK, Json(json!({"status": "updated", "tier": tier}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("save failed: {e}")})),
        )
            .into_response(),
    }
}

pub async fn save_config(State(state): State<AppState>) -> Response {
    let snapshot = state.config.snapshot().await;
    match state.config.save_to_disk(&snapshot).await {
        Ok(_) => (StatusCode::OK, Json(json!({"status": "saved"}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("save failed: {e}")})),
        )
            .into_response(),
    }
}

/// Lists available `ProviderType` values + the canonical aliases the
/// UI uses to populate the dropdown.
pub async fn list_provider_types() -> Json<serde_json::Value> {
    let types: Vec<&str> = vec![
        "anthropic",
        "google",
        "kiro",
        "responses",
        "generic",
        "openai",
        "openrouter",
        "tokenrouter",
        "groq",
        "deepseek",
        "fireworks",
        "mistral",
        "xai",
        "qwen",
        "moonshot",
        "zai",
        "xiaomi",
        "minimax",
        "byteplus",
        "nvidia",
        "opencode-go",
        "opencode-zen",
        "kilo",
        "commandcode",
        "github-copilot",
        "gitlawb",
        "ollama",
        "ollama-cloud",
        "llamacpp",
        "lmstudio",
    ];
    Json(
        json!({"types": types, "aliases": serde_json::to_value(crate::providers::manifest::ALIASES).unwrap_or(json!([]))}),
    )
}

/// Verify a provider type string parses. Used by the UI form to
/// surface validation errors without round-tripping a full add.
pub async fn validate_provider_type(Json(body): Json<serde_json::Value>) -> Response {
    let s = body
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if s.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "type field required"})),
        )
            .into_response();
    }
    if crate::providers::resolve_alias(s).is_some() {
        (StatusCode::OK, Json(json!({"status": "ok"}))).into_response()
    } else {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("unknown provider type: {s}")})),
        )
            .into_response()
    }
}
