//! Login + setup pages. Server-rendered HTML, no Node build.
//!
//! /ui/login — accept API key OR email+password. Set the session
//! cookie, redirect to the dashboard.
//! /ui/setup — shown when there are no users in the DB. Create
//! the first admin user, then redirect to /ui/login.

use crate::auth::Role;
use crate::server::auth as mw;
use crate::server::ui::layout;
use crate::server::AppState;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Form;
use serde::Deserialize;

pub async fn login_page(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    // If users exist AND we have a session, redirect to dashboard.
    if let Some(redirect) = params.get("redirect") {
        if !redirect.is_empty() {
            // Stash redirect in a cookie? For now, pass via query string.
        }
    }
    // Check if any users exist. If not, redirect to /ui/setup.
    match state.user_store.list_users().await {
        Ok(users) if users.is_empty() => {
            return axum::response::Redirect::to("/ui/setup").into_response();
        }
        _ => {}
    }
    let body = r##"
<h1>Sign in</h1>
<p class="dim">Sign in with your API key or email + password.</p>

<div id="login-panel" class="wizard-panel">
  <div class="row">
    <button id="tab-key" class="secondary">API key</button>
    <button id="tab-pw" class="secondary">Email + password</button>
  </div>

  <form id="form-key" hx-post="/auth/login" hx-target="#login-result" hx-swap="innerHTML">
    <label>API key</label>
    <input name="api_key" type="password" placeholder="tk-…" autofocus />
    <button type="submit">Sign in with API key</button>
  </form>

  <form id="form-pw" hx-post="/auth/login" hx-target="#login-result" hx-swap="innerHTML" style="display:none">
    <label>Email</label>
    <input name="email" type="email" />
    <label>Password</label>
    <input name="password" type="password" />
    <button type="submit">Sign in</button>
  </form>

  <div id="login-result"></div>
</div>

<script>
(function() {
  const tabKey = document.getElementById('tab-key');
  const tabPw = document.getElementById('tab-pw');
  const formKey = document.getElementById('form-key');
  const formPw = document.getElementById('form-pw');
  function show(which) {
    formKey.style.display = which === 'key' ? '' : 'none';
    formPw.style.display = which === 'pw' ? '' : 'none';
    tabKey.classList.toggle('active', which === 'key');
    tabPw.classList.toggle('active', which === 'pw');
  }
  show('key');
  tabKey.addEventListener('click', () => show('key'));
  tabPw.addEventListener('click', () => show('pw'));
})();
</script>
"##;
    let mut html = layout("login", "Sign in", body, None);
    html = html.replace(
        "<title>",
        "<meta name=\"referrer\" content=\"no-referrer\" /><title>",
    );
    let mut resp = (StatusCode::OK, axum::response::Html(html)).into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    resp
}

#[derive(Deserialize)]
pub struct LoginForm {
    pub api_key: Option<String>,
    pub email: Option<String>,
    pub password: Option<String>,
}

impl From<LoginForm> for crate::server::auth_endpoints::LoginReq {
    fn from(f: LoginForm) -> Self {
        Self {
            api_key: f.api_key,
            email: f.email,
            password: f.password,
        }
    }
}

/// HTMX form fallback for the login page (when JS isn't loaded).
pub async fn login_form(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Form(body): Form<LoginForm>,
) -> Response {
    let req: crate::server::auth_endpoints::LoginReq = body.into();
    crate::server::auth_endpoints::login(State(state), headers, axum::Json(req)).await
}

pub async fn setup_page(State(state): State<AppState>) -> Response {
    // If users exist, redirect to login.
    match state.user_store.list_users().await {
        Ok(users) if !users.is_empty() => {
            return axum::response::Redirect::to("/ui/login").into_response();
        }
        _ => {}
    }
    let body = r##"
<h1>Welcome — create your admin account</h1>
<p class="dim">First-run setup. The first user becomes the admin.</p>

<form method="POST" action="/ui/setup" class="wizard-panel">
  <div class="row three">
    <div>
      <label>Email</label>
      <input name="email" type="email" required autofocus />
    </div>
    <div>
      <label>Display name</label>
      <input name="name" required />
    </div>
    <div>
      <label>Password</label>
      <input name="password" type="password" required minlength="8" />
    </div>
  </div>
  <div class="actions">
    <button type="submit">Create admin account</button>
  </div>
</form>
"##;
    let html = layout("setup", "Setup", body, None);
    let mut resp = (StatusCode::OK, axum::response::Html(html)).into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    resp
}

/// POST /ui/setup — creates the first admin user.
pub async fn setup_submit(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Form(body): Form<LoginForm>,
) -> Response {
    // Refuse if users already exist (only the first setup is allowed).
    match state.user_store.list_users().await {
        Ok(users) if !users.is_empty() => {
            return axum::response::Redirect::to("/ui/login").into_response();
        }
        _ => {}
    }
    let email = match body.email.as_ref().filter(|s| !s.is_empty()) {
        Some(e) => e.clone(),
        None => {
            return (StatusCode::BAD_REQUEST, "email required").into_response();
        }
    };
    let name = email.split('@').next().unwrap_or("admin").to_string();
    let password = body.password.as_deref().unwrap_or("");
    if password.len() < 8 {
        return (StatusCode::BAD_REQUEST, "password must be at least 8 chars").into_response();
    }
    let user = match state
        .user_store
        .create_user(&email, &name, Some(password), Role::Admin)
        .await
    {
        Ok(u) => u,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("create: {e}")).into_response();
        }
    };
    // Auto-login: create session + set cookie.
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let plaintext = mw::create_session_cookie(&state, &user.id, user_agent.as_deref(), None)
        .await
        .unwrap_or_default();
    let mut resp = axum::response::Redirect::to("/ui/login?setup=ok").into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        format!(
            "{}={}; HttpOnly; SameSite=Lax; Path=/; Max-Age=2592000",
            mw::session_cookie_name(),
            plaintext
        )
        .parse()
        .unwrap(),
    );
    resp
}
