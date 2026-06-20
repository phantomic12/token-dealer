//! Auth-related endpoints: login, logout, signup (admin), me.
//!
//! These are called from both the WebUI (cookies) and external
//! clients (Bearer API key). The login form on /ui/login accepts
//! either an API key OR email+password.

use crate::auth::{
    generate_session_token, hash_password, sha256_hex, verify_password, ApiKey, Role, User,
    UserContext, UserStore,
};
use crate::server::auth as mw;
use crate::server::AppState;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct LoginReq {
    /// One of: an API key (starts with "tk-") OR email + password.
    pub api_key: Option<String>,
    pub email: Option<String>,
    pub password: Option<String>,
}

impl Clone for LoginReq {
    fn clone(&self) -> Self {
        Self {
            api_key: self.api_key.clone(),
            email: self.email.clone(),
            password: self.password.clone(),
        }
    }
}

/// POST /auth/login — accepts an API key OR email+password.
/// Returns 200 with a Set-Cookie header on success.
pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoginReq>,
) -> Response {
    // Path 1: API key login. Verify the key, create a session.
    if let Some(key) = &body.api_key {
        if !key.is_empty() {
            if let Some((user, _api_key)) = state
                .user_store
                .get_user_by_api_key(key)
                .await
                .ok()
                .flatten()
            {
                return finish_login(&state, &headers, &user).await;
            }
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "invalid API key"})),
            )
                .into_response();
        }
    }
    // Path 2: email + password.
    if let (Some(email), Some(password)) = (&body.email, &body.password) {
        let user = match state.user_store.get_user_by_email(email).await {
            Ok(Some(u)) => u,
            _ => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({"error": "invalid credentials"})),
                )
                    .into_response();
            }
        };
        let Some(hash) = user.password_hash.as_ref() else {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "password login not enabled for this user"})),
            )
                .into_response();
        };
        if !verify_password(password, hash) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "invalid credentials"})),
            )
                .into_response();
        }
        return finish_login(&state, &headers, &user).await;
    }
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({"error": "provide api_key OR email+password"})),
    )
        .into_response()
}

async fn finish_login(state: &AppState, headers: &HeaderMap, user: &User) -> Response {
    let _ = state.user_store.touch_last_login(&user.id).await;
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let ip = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(String::from);
    let plaintext = match mw::create_session_cookie(
        state,
        &user.id,
        user_agent.as_deref(),
        ip.as_deref(),
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("session: {e}")})),
            )
                .into_response();
        }
    };
    // Set HttpOnly + Secure (in production) cookie.
    let cookie_value = format!(
        "{}={}; HttpOnly; SameSite=Lax; Path=/; Max-Age=2592000",
        mw::session_cookie_name(),
        plaintext
    );
    let mut resp = (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "ok",
            "user": {
                "id": user.id,
                "email": user.email,
                "name": user.name,
                "role": user.role.as_str(),
            }
        })),
    )
        .into_response();
    resp.headers_mut()
        .insert(header::SET_COOKIE, cookie_value.parse().unwrap());
    resp
}

/// POST /auth/logout — clears the session cookie + invalidates the
/// session row in the DB.
pub async fn logout(
    State(state): State<AppState>,
    axum::extract::Extension(user): axum::extract::Extension<UserContext>,
) -> Response {
    if let Some(sid) = &user.session_id {
        let _ = state.user_store.delete_session(sid).await;
    }
    let mut resp = (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        format!(
            "{}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0",
            mw::session_cookie_name()
        )
        .parse()
        .unwrap(),
    );
    resp
}

/// GET /auth/me — returns the current user context.
pub async fn me(axum::extract::Extension(user): axum::extract::Extension<UserContext>) -> Response {
    Json(serde_json::json!({
        "id": user.user_id,
        "email": user.email,
        "name": user.name,
        "role": user.role.as_str(),
        "via": user.via,
        "key_prefix": user.key_prefix,
    }))
    .into_response()
}

// ── Admin: user management ─────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateUserReq {
    pub email: String,
    pub name: String,
    pub password: Option<String>,
    /// Defaults to "user". Use "admin" for elevated access.
    pub role: Option<String>,
}

/// POST /admin/users — admin-only. Creates a user.
pub async fn create_user(
    State(state): State<AppState>,
    axum::extract::Extension(caller): axum::extract::Extension<UserContext>,
    Json(body): Json<CreateUserReq>,
) -> Response {
    if !caller.is_admin() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "admin only"})),
        )
            .into_response();
    }
    let role = body.role.as_deref().map(Role::parse).unwrap_or(Role::User);
    match state
        .user_store
        .create_user(&body.email, &body.name, body.password.as_deref(), role)
        .await
    {
        Ok(u) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "id": u.id,
                "email": u.email,
                "name": u.name,
                "role": u.role.as_str(),
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// GET /admin/users — admin-only. Lists all users.
pub async fn list_users(
    State(state): State<AppState>,
    axum::extract::Extension(caller): axum::extract::Extension<UserContext>,
) -> Response {
    if !caller.is_admin() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "admin only"})),
        )
            .into_response();
    }
    match state.user_store.list_users().await {
        Ok(users) => {
            let arr: Vec<_> = users
                .iter()
                .map(|u| {
                    serde_json::json!({
                        "id": u.id,
                        "email": u.email,
                        "name": u.name,
                        "role": u.role.as_str(),
                        "created_at": u.created_at.to_rfc3339(),
                        "last_login_at": u.last_login_at.map(|d| d.to_rfc3339()),
                    })
                })
                .collect();
            (StatusCode::OK, Json(serde_json::json!({"users": arr}))).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// DELETE /admin/users/:id — admin-only.
pub async fn delete_user(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Extension(caller): axum::extract::Extension<UserContext>,
) -> Response {
    if !caller.is_admin() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "admin only"})),
        )
            .into_response();
    }
    match state.user_store.delete_user(&id).await {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "deleted"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST /auth/keys — creates a new API key for the calling user.
pub async fn create_own_key(
    State(state): State<AppState>,
    axum::extract::Extension(user): axum::extract::Extension<UserContext>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("default")
        .to_string();
    match state.user_store.create_api_key(&user.user_id, &name).await {
        Ok((_key, plaintext)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "api_key": plaintext,
                "warning": "Save this key — it won't be shown again.",
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// GET /auth/keys — list current user's keys.
pub async fn list_own_keys(
    State(state): State<AppState>,
    axum::extract::Extension(user): axum::extract::Extension<UserContext>,
) -> Response {
    match state.user_store.list_api_keys(&user.user_id).await {
        Ok(keys) => {
            let arr: Vec<_> = keys
                .iter()
                .map(|k| {
                    serde_json::json!({
                        "id": k.id,
                        "name": k.name,
                        "prefix": k.key_prefix,
                        "created_at": k.created_at.to_rfc3339(),
                        "last_used_at": k.last_used_at.map(|d| d.to_rfc3339()),
                        "revoked": k.revoked,
                    })
                })
                .collect();
            (StatusCode::OK, Json(serde_json::json!({"keys": arr}))).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// DELETE /auth/keys/:id — revoke a key.
pub async fn delete_own_key(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Extension(user): axum::extract::Extension<UserContext>,
) -> Response {
    // Verify the key belongs to this user before deleting.
    let keys = state
        .user_store
        .list_api_keys(&user.user_id)
        .await
        .unwrap_or_default();
    let owned = keys.iter().any(|k| k.id == id);
    if !owned {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "not found"})),
        )
            .into_response();
    }
    match state.user_store.delete_api_key(&id).await {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "deleted"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// GET /auth/usage — returns the calling user's usage summary.
pub async fn my_usage(
    State(state): State<AppState>,
    axum::extract::Extension(user): axum::extract::Extension<UserContext>,
) -> Response {
    let today = state
        .user_store
        .get_usage_today(&user.user_id)
        .await
        .unwrap_or((0, 0, 0.0, 0));
    let last30 = state
        .user_store
        .get_usage_summary(&user.user_id, 30)
        .await
        .unwrap_or_default();
    Json(serde_json::json!({
        "today": {
            "input_tokens": today.0,
            "output_tokens": today.1,
            "cost_usd": today.2,
            "request_count": today.3,
        },
        "last_30_days": last30.iter().map(|(day, input, output, cost)| {
            serde_json::json!({
                "day": day,
                "input_tokens": input,
                "output_tokens": output,
                "cost_usd": cost,
            })
        }).collect::<Vec<_>>(),
    }))
    .into_response()
}

// ── Admin: create API key for any user ────────────────────────────

#[derive(Deserialize)]
pub struct AdminCreateKeyReq {
    pub user_id: String,
    pub name: String,
}

/// POST /admin/users/:id/keys — admin-only. Create an API key
/// for a specific user (used for service accounts). The `:id` is
/// the user id; use the special path `__self__` to send the user id
/// in the body instead (handled by the JS on /ui/users).
pub async fn admin_create_key(
    State(state): State<AppState>,
    axum::extract::Path(user_id_path): axum::extract::Path<String>,
    axum::extract::Extension(caller): axum::extract::Extension<UserContext>,
    axum::Json(body): axum::Json<AdminCreateKeyReq>,
) -> Response {
    if !caller.is_admin() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "admin only"})),
        )
            .into_response();
    }
    // If the path is "__self__" the user_id comes from the body.
    let target = if user_id_path == "__self__" {
        body.user_id.clone()
    } else {
        user_id_path
    };
    if target.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "user_id required"})),
        )
            .into_response();
    }
    match state.user_store.create_api_key(&target, &body.name).await {
        Ok((_key, plaintext)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "api_key": plaintext,
                "user_id": target,
                "warning": "Save this key — it won't be shown again.",
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}
