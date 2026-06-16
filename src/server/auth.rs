//! Inbound authentication. Optional via `[auth] enabled = true`.
//!
//! API routes (`/v1/*`): `Authorization: Bearer <key>`.
//! UI/admin routes (`/ui/*`, `/admin/*`): HTTP Basic with the same key.
//! `/health` and static CSS/JS are always public.
//!
//! Same key table for both — users can `rtr_<random>` their own keys
//! and share them with anything that talks to the router.

use super::AppState;
use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::Engine;

#[derive(Clone)]
pub struct AuthKey {
    pub key: String,
    pub name: String,
}

pub fn keys_from_config(cfg: &crate::config::types::AuthConfig) -> Vec<AuthKey> {
    cfg.keys
        .iter()
        .map(|k| AuthKey {
            key: resolve(k.key.as_str()),
            name: k.name.clone(),
        })
        .filter(|k| !k.key.is_empty())
        .collect()
}

fn resolve(literal: &str) -> String {
    if let Some(inner) = literal.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        if let Ok(v) = std::env::var(inner) {
            return v;
        }
    }
    literal.to_string()
}

fn check(keys: &[AuthKey], presented: &str) -> bool {
    if presented.is_empty() {
        return false;
    }
    keys.iter().any(|k| constant_time_eq(k.key.as_bytes(), presented.as_bytes()))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub async fn middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let snap = state.config.snapshot().await;
    if !snap.auth.enabled {
        return next.run(req).await;
    }
    let keys = keys_from_config(&snap.auth);
    if keys.is_empty() {
        // enabled but no keys configured — fail closed
        return unauthorized("auth enabled but no keys configured");
    }

    let path = req.uri().path().to_string();
    let public = is_public_path(&path);
    if public {
        return next.run(req).await;
    }

    let presented = extract_token(&req);
    if !check(&keys, &presented) {
        return unauthorized("invalid or missing API key");
    }
    next.run(req).await
}

fn is_public_path(path: &str) -> bool {
    path == "/health"
        || path == "/v1/health"
        || path == "/ui/style.css"
}

fn extract_token(req: &Request) -> String {
    // Try Authorization: Bearer ...
    if let Some(h) = req.headers().get(header::AUTHORIZATION) {
        if let Ok(s) = h.to_str() {
            if let Some(rest) = s.strip_prefix("Bearer ") {
                return rest.trim().to_string();
            }
            if let Some(rest) = s.strip_prefix("Basic ") {
                if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(rest.trim()) {
                    if let Ok(s) = String::from_utf8(decoded) {
                        if let Some((_, p)) = s.split_once(':') {
                            return p.to_string();
                        }
                    }
                }
            }
            return s.to_string();
        }
    }
    String::new()
}

fn unauthorized(msg: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE,
            r#"Basic realm="token-dealer", charset="UTF-8""#,
        )],
        axum::Json(serde_json::json!({
            "error": {
                "message": msg,
                "type": "invalid_request_error",
                "code": 401,
            }
        })),
    )
        .into_response()
}
