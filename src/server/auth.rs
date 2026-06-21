//! Auth middleware.
//!
//! Resolution order on a request:
//!   1. `Authorization: Bearer tk-***` → user via api_keys (multi-tenant)
//!   2. `td_session` cookie → user via sessions (WebUI)
//!   3. Legacy: `Authorization: Bearer ***` (or Basic) → admin via
//!      [auth].admin_key / `TOKEN_DEALER_ADMIN_PASSWORD` env var
//!   4. None → anonymous. Anonymous is allowed on public paths
//!      (health, /v1/models, /v1/stats, /ui/style.css, /ui/login,
//!      /ui/setup) and rejected on /v1/* with 401. /ui/* is allowed
//!      for anonymous; the UI shows a "log in" banner.
//!
//! On success, attaches `UserContext` to request extensions so
//! handlers can read it via `Extension<UserContext>`.

use crate::auth::{Role, UserContext};
use crate::server::AppState;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine as _;

const SESSION_COOKIE: &str = "td_session";
const SESSION_TTL_HOURS: i64 = 24 * 30; // 30 days

pub async fn middleware(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let snap = state.config.snapshot().await;

    // Try multi-user API key first.
    let presented = extract_token(&req);
    if !presented.is_empty() {
        if presented.starts_with("tk-") {
            if let Some((user, key)) = state
                .user_store
                .get_user_by_api_key(&presented)
                .await
                .ok()
                .flatten()
            {
                let ctx = UserContext {
                    user_id: user.id,
                    email: user.email,
                    name: user.name,
                    role: user.role,
                    via: "api_key",
                    key_prefix: Some(key.key_prefix),
                    session_id: None,
                };
                req.extensions_mut().insert(ctx);
                return next.run(req).await;
            }
        }

        // Legacy config-defined keys.
        if snap.auth.enabled {
            let keys = keys_from_config(&snap.auth);
            if !keys.is_empty() && check(&keys, &presented) {
                let ctx = UserContext {
                    user_id: "legacy".to_string(),
                    email: "legacy@admin".to_string(),
                    name: "Legacy admin".to_string(),
                    role: Role::Admin,
                    via: "legacy_key",
                    key_prefix: None,
                    session_id: None,
                };
                req.extensions_mut().insert(ctx);
                return next.run(req).await;
            }
        }
        // Legacy env-var password.
        if let Ok(admin_pw) = std::env::var("TOKEN_DEALER_ADMIN_PASSWORD") {
            if !admin_pw.is_empty() && constant_time_eq(presented.as_bytes(), admin_pw.as_bytes()) {
                let ctx = UserContext {
                    user_id: "env-admin".to_string(),
                    email: "admin@env".to_string(),
                    name: "Env admin".to_string(),
                    role: Role::Admin,
                    via: "env_password",
                    key_prefix: None,
                    session_id: None,
                };
                req.extensions_mut().insert(ctx);
                return next.run(req).await;
            }
        }
    }

    // Try session cookie (WebUI).
    if let Some(cookie) = req
        .headers()
        .get(header::COOKIE)
        .and_then(|h| h.to_str().ok())
    {
        for (k, v) in parse_cookies(cookie) {
            if k == SESSION_COOKIE {
                if let Some((session, user)) = state.user_store.get_session(&v).await.ok().flatten()
                {
                    let ctx = UserContext {
                        user_id: user.id,
                        email: user.email,
                        name: user.name,
                        role: user.role,
                        via: "session",
                        key_prefix: None,
                        session_id: Some(session.id),
                    };
                    req.extensions_mut().insert(ctx);
                    return next.run(req).await;
                }
            }
        }
    }

    // No credentials → anonymous.
    let path = req.uri().path().to_string();
    // Backwards compat: when auth is disabled in config, all
    // requests pass through (single-tenant test/dev mode). The
    // existing tests rely on this — they don't set auth.enabled.
    if !snap.auth.enabled {
        req.extensions_mut().insert(UserContext {
            user_id: "anonymous".to_string(),
            email: "anonymous@local".to_string(),
            name: "Anonymous".to_string(),
            role: Role::User,
            via: "auth_disabled",
            key_prefix: None,
            session_id: None,
        });
        return next.run(req).await;
    }
    if is_public_path(&path) {
        req.extensions_mut().insert(UserContext {
            user_id: "anonymous".to_string(),
            email: "anonymous@local".to_string(),
            name: "Anonymous".to_string(),
            role: Role::User,
            via: "public",
            key_prefix: None,
            session_id: None,
        });
        return next.run(req).await;
    }
    if path.starts_with("/v1/") && req.method() != axum::http::Method::OPTIONS {
        return unauthorized("invalid or missing API key");
    }
    // v0.2.0 plan item 1 (the previously-deferred piece):
    // when `[auth] enabled = true`, /ui/* and /admin/* also
    // require credentials. The plan text is exact: "all /ui/*
    // and /admin/* routes require auth when [auth] enabled".
    //
    // Browser requests get a 303 redirect to the login page so
    // the user lands somewhere useful; API-style requests (Accept:
    // application/json or no Accept header on an /admin/* path)
    // get a 401 with a JSON error envelope.
    if path.starts_with("/ui/") || path.starts_with("/admin/") {
        return ui_or_admin_unauthorized(&req, &path);
    }
    // Anonymous UI access — attach a UserContext anyway so handlers
    // can rely on it being present.
    req.extensions_mut().insert(UserContext {
        user_id: "anonymous".to_string(),
        email: "anonymous@local".to_string(),
        name: "Anonymous".to_string(),
        role: Role::User,
        via: "anonymous",
        key_prefix: None,
        session_id: None,
    });
    next.run(req).await
}

/// 401 / 303 for unauthenticated `/ui/*` + `/admin/*` requests
/// when `[auth] enabled = true`. Browser-style requests (no
/// explicit `Accept: application/json`) get a 303 redirect to
/// `/ui/login?next=<path>` so the login flow can return the user
/// to where they were going. JSON clients get a 401 + envelope
/// instead of a redirect they can't follow.
fn ui_or_admin_unauthorized(req: &Request, path: &str) -> Response {
    let wants_json = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.contains("application/json"))
        .unwrap_or(false);
    if wants_json {
        return unauthorized("invalid or missing credentials");
    }
    // 303 See Other so the browser follows the login form, then
    // comes back to where the user was trying to go.
    let next = urlencoding_minimal(path);
    let location = format!("/ui/login?next={next}");
    let mut resp = (StatusCode::SEE_OTHER, "").into_response();
    resp.headers_mut()
        .insert(header::LOCATION, location.parse().unwrap());
    resp
}

/// Minimal percent-encoding for the `next` redirect param.
/// Avoids pulling in the full `urlencoding` crate for one
/// use site. Handles the only chars that appear in our
/// /ui/* and /admin/* paths: slashes, colons, query strings.
fn urlencoding_minimal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' | '/' | '?' | '=' | '&' => {
                out.push(c);
            }
            _ => {
                let mut buf = [0u8; 4];
                let bytes = c.encode_utf8(&mut buf);
                for b in bytes.bytes() {
                    out.push_str(&format!("%{:02X}", b));
                }
            }
        }
    }
    out
}

/// Helper: create a new session for a user, return the cookie value.
pub async fn create_session_cookie(
    state: &AppState,
    user_id: &str,
    user_agent: Option<&str>,
    ip: Option<&str>,
) -> anyhow::Result<String> {
    let (_session, plaintext) = state
        .user_store
        .create_session(user_id, user_agent, ip, SESSION_TTL_HOURS)
        .await?;
    Ok(plaintext)
}

pub fn session_cookie_name() -> &'static str {
    SESSION_COOKIE
}

pub fn is_public_path(path: &str) -> bool {
    path == "/health"
        || path == "/v1/health"
        || path == "/ui/style.css"
        || path == "/v1/stats"
        || path == "/ui/login"
        || path == "/ui/login.html"
        || path == "/ui/setup"
        || path == "/v1/provider-types"
        || path == "/admin/oauth/callback"
        || path == "/admin/healthz"
}

fn extract_token(req: &Request) -> String {
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

fn parse_cookies(cookie_header: &str) -> Vec<(String, String)> {
    cookie_header
        .split(';')
        .filter_map(|s| {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            let (k, v) = s.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

fn unauthorized(msg: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE,
            r#"Basic realm="token-dealer", charset="UTF-8""#,
        )],
        Json(serde_json::json!({
            "error": {
                "message": msg,
                "type": "invalid_request_error",
                "code": 401,
            }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod ui_auth_tests {
    use super::*;

    #[test]
    fn url_encoding_handles_safe_chars() {
        // Reserved-but-safe chars survive unescaped.
        let out = urlencoding_minimal("/ui/providers?foo=bar&baz=1");
        assert_eq!(out, "/ui/providers?foo=bar&baz=1");
    }

    #[test]
    fn url_encoding_escapes_unsafe_chars() {
        // Spaces, quotes, etc. become %XX.
        let out = urlencoding_minimal("/ui/x y");
        assert_eq!(out, "/ui/x%20y");
        let out = urlencoding_minimal("/ui/a\"b");
        assert_eq!(out, "/ui/a%22b");
    }

    #[test]
    fn url_encoding_handles_unicode() {
        // Multi-byte UTF-8 encoded byte-by-byte.
        let out = urlencoding_minimal("/ui/café");
        // "café" → c, a, f, %C3, %A9
        assert_eq!(out, "/ui/caf%C3%A9");
    }
}

fn keys_from_config(cfg: &crate::config::types::AuthConfig) -> Vec<String> {
    let mut out = Vec::new();
    // Legacy single admin key.
    if let Some(k) = &cfg.admin_key {
        if !k.is_empty() {
            out.push(k.clone());
        }
    }
    // Multi-key list (newer).
    for k in &cfg.keys {
        if !k.key.is_empty() {
            out.push(k.key.clone());
        }
    }
    if let Ok(env) = std::env::var("ROUTER_MASTER_KEY") {
        if !env.is_empty() {
            out.push(env);
        }
    }
    out
}

fn check(keys: &[String], presented: &str) -> bool {
    if presented.is_empty() {
        return false;
    }
    for k in keys {
        if constant_time_eq(k.as_bytes(), presented.as_bytes()) {
            return true;
        }
    }
    false
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
