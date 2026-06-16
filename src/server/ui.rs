//! WebUI — server-rendered HTML + HTMX for interactivity.
//! Three pages: Dashboard, Providers, Tiers. No build step, no Node
//! toolchain, no separate static directory. Everything is generated
//! from Rust + an embedded stylesheet. HTMX loads from a CDN.

use super::AppState;
use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
};
use std::fmt::Write;

const CSS: &str = r##"
:root {
  --bg: #0e1116;
  --bg-elev: #161b22;
  --bg-elev-2: #1c232c;
  --border: #2a3441;
  --text: #e6edf3;
  --text-dim: #8b949e;
  --accent: #58a6ff;
  --accent-dim: #1f6feb;
  --green: #3fb950;
  --yellow: #d29922;
  --red: #f85149;
  --mono: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
  --sans: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
}
* { box-sizing: border-box; }
html, body { margin: 0; padding: 0; background: var(--bg); color: var(--text);
  font-family: var(--sans); font-size: 14px; line-height: 1.5; }
a { color: var(--accent); text-decoration: none; }
a:hover { text-decoration: underline; }
header.app {
  display: flex; align-items: center; justify-content: space-between;
  padding: 12px 24px; background: var(--bg-elev); border-bottom: 1px solid var(--border);
}
header.app .brand { font-family: var(--mono); font-weight: 600; font-size: 16px;
  color: var(--text); }
header.app nav { display: flex; gap: 18px; }
header.app nav a { color: var(--text-dim); font-weight: 500; padding: 4px 0;
  border-bottom: 2px solid transparent; }
header.app nav a.active { color: var(--text); border-bottom-color: var(--accent); }
header.app nav a:hover { color: var(--text); text-decoration: none; }
header.app .actions { display: flex; gap: 8px; }
main { max-width: 1100px; margin: 24px auto; padding: 0 24px 64px; }
h1 { font-size: 22px; margin: 0 0 16px; font-weight: 600; }
h2 { font-size: 16px; margin: 24px 0 12px; font-weight: 600; color: var(--text-dim);
  text-transform: uppercase; letter-spacing: 0.05em; }
p.dim { color: var(--text-dim); margin: 0 0 16px; }
.cards { display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
  gap: 16px; margin-bottom: 24px; }
.card { background: var(--bg-elev); border: 1px solid var(--border);
  border-radius: 6px; padding: 16px; }
.card .label { color: var(--text-dim); font-size: 12px;
  text-transform: uppercase; letter-spacing: 0.05em; }
.card .value { font-size: 24px; font-weight: 600; margin-top: 4px; font-family: var(--mono); }
.card .value.green { color: var(--green); }
.card .value.yellow { color: var(--yellow); }
.card .value.red { color: var(--red); }
table { width: 100%; border-collapse: collapse; background: var(--bg-elev);
  border: 1px solid var(--border); border-radius: 6px; overflow: hidden; }
th, td { text-align: left; padding: 10px 14px; border-bottom: 1px solid var(--border);
  font-family: var(--mono); font-size: 13px; vertical-align: middle; }
th { background: var(--bg-elev-2); color: var(--text-dim); font-weight: 500;
  text-transform: uppercase; font-size: 11px; letter-spacing: 0.05em; }
tr:last-child td { border-bottom: none; }
tr:hover td { background: rgba(255, 255, 255, 0.02); }
.badge { display: inline-block; padding: 2px 8px; border-radius: 10px;
  font-size: 11px; font-family: var(--mono); font-weight: 500; }
.badge.healthy { background: rgba(63, 185, 80, 0.15); color: var(--green); }
.badge.degraded { background: rgba(210, 153, 34, 0.15); color: var(--yellow); }
.badge.down { background: rgba(248, 81, 73, 0.15); color: var(--red); }
.badge.local { background: rgba(88, 166, 255, 0.15); color: var(--accent); }
button, .btn { background: var(--accent-dim); color: white; border: none;
  padding: 6px 14px; border-radius: 5px; font-size: 13px; cursor: pointer;
  font-family: var(--sans); font-weight: 500; }
button:hover, .btn:hover { background: var(--accent); }
button.secondary { background: transparent; color: var(--text);
  border: 1px solid var(--border); }
button.secondary:hover { background: var(--bg-elev-2); }
button.danger { background: var(--red); }
button.danger:hover { background: #ff6b62; }
input, select, textarea { background: var(--bg); color: var(--text);
  border: 1px solid var(--border); border-radius: 5px; padding: 6px 10px;
  font-family: var(--mono); font-size: 13px; width: 100%; }
input:focus, select:focus, textarea:focus { outline: none; border-color: var(--accent); }
label { display: block; font-size: 12px; color: var(--text-dim); margin: 12px 0 4px;
  text-transform: uppercase; letter-spacing: 0.04em; }
form .row { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
form .row.three { grid-template-columns: 1fr 1fr 1fr; }
form .actions { margin-top: 16px; display: flex; gap: 8px; align-items: center; }
.flash { padding: 10px 14px; border-radius: 5px; margin-bottom: 16px;
  font-size: 13px; }
.flash.success { background: rgba(63, 185, 80, 0.15); color: var(--green);
  border: 1px solid rgba(63, 185, 80, 0.3); }
.flash.error { background: rgba(248, 81, 73, 0.15); color: var(--red);
  border: 1px solid rgba(248, 81, 73, 0.3); }
.notice { background: rgba(210, 153, 34, 0.1); border: 1px solid rgba(210, 153, 34, 0.3);
  border-radius: 5px; padding: 10px 14px; margin-bottom: 16px; font-size: 13px;
  color: var(--yellow); }
.kbd { font-family: var(--mono); font-size: 12px; padding: 1px 6px;
  background: var(--bg-elev-2); border: 1px solid var(--border); border-radius: 3px; }
.muted { color: var(--text-dim); }
.center-empty { text-align: center; padding: 48px 16px; color: var(--text-dim); }
form.inline { display: flex; gap: 6px; align-items: center; }
form.inline input, form.inline select { width: auto; min-width: 120px; }
"##;

const HTMX_URL: &str = "https://unpkg.com/htmx.org@1.9.10";

fn layout(active: &str, title: &str, body: &str, flash: Option<&str>) -> String {
    let nav = |href: &str, label: &str, key: &str| -> String {
        let cls = if active == key { "active" } else { "" };
        format!(r##"<a href="{href}" class="{cls}">{label}</a>"##)
    };
    let flash_html = match flash {
        Some(f) => format!(r##"<div id="flash" class="flash">{f}</div>"##),
        None => String::new(),
    };
    format!(
        r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>{title} · token-dealer</title>
  <link rel="stylesheet" href="/ui/style.css" />
  <script src="{HTMX_URL}" defer></script>
</head>
<body>
<header class="app">
  <div class="brand">token-dealer</div>
  <nav>
    {nav_dashboard}
    {nav_providers}
    {nav_tiers}
  </nav>
  <div class="actions">
    <button class="secondary" hx-post="/admin/config/save" hx-swap="none" hx-on::after-request="document.getElementById('flash')?.remove();let f=document.createElement('div');f.id='flash';f.className='flash success';f.textContent='saved to disk';document.querySelector('main').prepend(f);setTimeout(()=>f.remove(),3000)">Save to disk</button>
  </div>
</header>
<main>
  {flash_html}
  {body}
</main>
</body>
</html>"##,
        title = title,
        HTMX_URL = HTMX_URL,
        nav_dashboard = nav("/", "Dashboard", "dashboard"),
        nav_providers = nav("/ui/providers", "Providers", "providers"),
        nav_tiers = nav("/ui/tiers", "Tiers", "tiers"),
        flash_html = flash_html,
        body = body,
    )
}

pub async fn index() -> Response {
    (StatusCode::FOUND, [("location", "/ui/")], "").into_response()
}

pub async fn dashboard(State(state): State<AppState>) -> Response {
    let snap = state.config.snapshot().await;
    let providers = state.pipeline.registry.list().await;
    let n_providers = providers.len();
    let n_tiers = snap.tiers.len();
    let bind = &snap.server.bind;

    let provider_rows: String = providers
        .iter()
        .map(|(id, model)| {
            format!(
                r##"<tr><td><a href="/ui/providers#{id}">{id}</a></td><td>{model}</td></tr>"##
            )
        })
        .collect();

    let body = format!(
        r##"
<h1>Dashboard</h1>
<p class="dim">Listening on <code class="kbd">{bind}</code> · config: <code class="kbd">{path}</code></p>

<div class="cards">
  <div class="card"><div class="label">Providers</div><div class="value">{n_providers}</div></div>
  <div class="card"><div class="label">Tiers</div><div class="value">{n_tiers}</div></div>
  <div class="card"><div class="label">Log</div><div class="value muted">phase 2</div></div>
</div>

<h2>Quick start</h2>
<p>Route to a specific model:</p>
<p><code class="kbd">curl -X POST http://{bind}/v1/chat/completions -d '{{"model":"anthropic/claude-sonnet-4-5","messages":[{{"role":"user","content":"hi"}}]}}'</code></p>
<p>Route by tier (uses the <code>standard</code> tier's primary):</p>
<p><code class="kbd">curl -X POST http://{bind}/v1/chat/completions -d '{{"model":"standard","messages":[{{"role":"user","content":"hi"}}]}}'</code></p>
<p>Force a tier via header:</p>
<p><code class="kbd">curl -X POST http://{bind}/v1/chat/completions -H "x-router-tier: complex" -d '{{...}}'</code></p>

<h2>Registered providers</h2>
<table>
  <thead><tr><th>ID</th><th>Default model</th></tr></thead>
  <tbody>{provider_rows}</tbody>
</table>
"##,
        bind = bind,
        path = state.config.path().display(),
        n_providers = n_providers,
        n_tiers = n_tiers,
        provider_rows = provider_rows,
    );

    Html(layout("dashboard", "Dashboard", &body, None)).into_response()
}

pub async fn ui_style() -> Response {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        CSS,
    )
        .into_response()
}

pub async fn providers_page(State(state): State<AppState>) -> Response {
    let snap = state.config.snapshot().await;
    let body = format!(
        r##"
<h1>Providers</h1>
<p class="dim">Adapters wired in to handle <code>provider/model</code> requests. The list and each adapter's defaults come from the manifest table; uncomment the matching block in <code>token-dealer.toml</code> to enable a provider for non-UI use.</p>
<div class="notice">Changes you make here are live in memory immediately and persisted to <code class="kbd">{path}</code>. Click "Save to disk" in the top right to force a flush.</div>

<h2>Add a provider</h2>
<form hx-post="/admin/providers" hx-target="#provider-list" hx-swap="outerHTML" hx-on::after-request="this.reset()">
  <div class="row">
    <div>
      <label>ID (used in <code>model</code> field)</label>
      <input name="id" placeholder="anthropic" required />
    </div>
    <div>
      <label>Type</label>
      <select name="type" id="provider-type-select" required>
        <option value="anthropic">anthropic</option>
        <option value="openai">openai</option>
        <option value="openrouter">openrouter</option>
        <option value="tokenrouter">tokenrouter</option>
        <option value="groq">groq</option>
        <option value="deepseek">deepseek</option>
        <option value="fireworks">fireworks</option>
        <option value="mistral">mistral</option>
        <option value="xai">xai</option>
        <option value="qwen">qwen</option>
        <option value="moonshot">moonshot</option>
        <option value="zai">zai</option>
        <option value="xiaomi">xiaomi</option>
        <option value="minimax">minimax</option>
        <option value="byteplus">byteplus</option>
        <option value="nvidia">nvidia</option>
        <option value="opencode-go">opencode-go</option>
        <option value="opencode-zen">opencode-zen</option>
        <option value="kilo">kilo</option>
        <option value="commandcode">commandcode</option>
        <option value="github-copilot">github-copilot</option>
        <option value="gitlawb">gitlawb</option>
        <option value="google">google</option>
        <option value="kiro">kiro</option>
        <option value="responses">responses</option>
        <option value="ollama">ollama</option>
        <option value="ollama-cloud">ollama-cloud</option>
        <option value="llamacpp">llamacpp</option>
        <option value="lmstudio">lmstudio</option>
        <option value="generic">generic</option>
      </select>
    </div>
  </div>
  <div class="row">
    <div>
      <label>API key (or <code>${{ENV_VAR}}</code>)</label>
      <input name="key" placeholder="${{ANTHROPIC_API_KEY}}" />
    </div>
    <div>
      <label>Base URL (optional — defaults from manifest)</label>
      <input name="base_url" placeholder="https://api.example.com" />
    </div>
  </div>
  <div class="row">
    <div>
      <label>Default model (optional)</label>
      <input name="default_model" placeholder="claude-sonnet-4-5" />
    </div>
    <div>
      <label>Path (optional)</label>
      <input name="path" placeholder="/v1/chat/completions" />
    </div>
  </div>
  <div class="actions">
    <button type="submit">Add provider</button>
    <span class="muted">Manifest defaults fill in base URL + path when left blank.</span>
  </div>
</form>

<h2>Configured providers</h2>
{list}
"##,
        path = state.config.path().display(),
        list = render_providers_list(&snap.providers).await,
    );

    Html(layout("providers", "Providers", &body, None)).into_response()
}

pub async fn providers_partial(State(state): State<AppState>) -> Response {
    let snap = state.config.snapshot().await;
    Html(render_providers_list(&snap.providers).await).into_response()
}

async fn render_providers_list(
    providers: &[crate::config::types::ProviderConfig],
) -> String {
    if providers.is_empty() {
        return r##"<div class="center-empty" id="provider-list">No providers configured. Add one above, or uncomment a block in <code>token-dealer.toml</code> and POST <code>/admin/config/reload</code>.</div>"##.to_string();
    }
    let mut rows = String::new();
    for p in providers {
        let local = matches!(
            p.provider_type,
            crate::config::types::ProviderType::Ollama
                | crate::config::types::ProviderType::LlamaCpp
                | crate::config::types::ProviderType::LmStudio
        );
        let badge = if local {
            r##"<span class="badge local">local</span>"##
        } else {
            r##"<span class="badge healthy">cloud</span>"##
        };
        let base = p.base_url.as_deref().unwrap_or("(manifest default)");
        let model = p.default_model.as_deref().unwrap_or("(manifest default)");
        let key_disp = match &p.key {
            Some(k) if k.starts_with("${") && k.ends_with('}') => {
                format!("<span class=\"muted\">env: {}</span>", &k[2..k.len() - 1])
            }
            Some(k) if !k.is_empty() => {
                let masked: String = "•".repeat(k.len().min(8));
                format!("<span class=\"kbd\">{masked}</span>")
            }
            _ => r##"<span class="muted">—</span>"##.to_string(),
        };
        let _ = write!(
            rows,
            r##"<tr id="provider-{id}">
              <td><strong>{id}</strong></td>
              <td><code>{type_name}</code> {badge}</td>
              <td>{base}</td>
              <td><code>{model}</code></td>
              <td>{key}</td>
              <td><form hx-post="/admin/ui/remove/{id}" hx-target="#provider-list" hx-swap="outerHTML" hx-confirm="Remove provider {id}?" class="inline"><button class="danger" type="submit">Remove</button></form></td>
            </tr>"##,
            id = html_escape(&p.id),
            type_name = format!("{:?}", p.provider_type).to_lowercase(),
            badge = badge,
            base = html_escape(base),
            model = html_escape(model),
            key = key_disp,
        );
    }
    format!(
        r##"<table id="provider-list">
  <thead><tr><th>ID</th><th>Type</th><th>Base URL</th><th>Default model</th><th>Key</th><th></th></tr></thead>
  <tbody>{rows}</tbody>
</table>"##
    )
}

/// Form-driven remove (HTMX form posts here, returns updated list).
pub async fn ui_remove_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let result = state
        .config
        .update_with(|cfg| {
            cfg.providers.retain(|p| p.id != id);
        })
        .await;
    match result {
        Ok(_) => {
            state.pipeline.registry.remove(&id).await;
            let snap = state.config.snapshot().await;
            Html(render_providers_list(&snap.providers).await).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Html(format!(r##"<div class="flash error">save failed: {}</div>"##, e)),
        )
            .into_response(),
    }
}

pub async fn tiers_page(State(state): State<AppState>) -> Response {
    let snap = state.config.snapshot().await;
    let body = format!(
        r##"
<h1>Tiers</h1>
<p class="dim">Per-tier primary model + fallbacks. Requests with <code>model: "tier"</code> or <code>x-router-tier: tier</code> route to that tier's primary; the first model in <code>fallbacks</code> is tried if the primary fails.</p>
<div class="notice">Edit the primary inline, click Save. "Save to disk" in the top right flushes to <code class="kbd">{path}</code>.</div>

{list}
"##,
        path = state.config.path().display(),
        list = render_tiers_list(&snap.tiers),
    );

    Html(layout("tiers", "Tiers", &body, None)).into_response()
}

fn render_tiers_list(
    tiers: &std::collections::HashMap<String, crate::config::types::TierConfig>,
) -> String {
    if tiers.is_empty() {
        return r##"<div class="center-empty">No tiers configured. Add a <code>[tiers.*]</code> block to <code>token-dealer.toml</code> and POST <code>/admin/config/reload</code>.</div>"##.to_string();
    }
    // Stable order: simple, standard, complex, reasoning, high_context, multimodal, then rest alpha.
    let order = ["simple", "standard", "complex", "reasoning", "high_context", "multimodal"];
    let mut keys: Vec<&String> = tiers.keys().collect();
    keys.sort_by_key(|k| {
        order
            .iter()
            .position(|o| *o == k.as_str())
            .unwrap_or(order.len() + k.as_str().len())
    });

    let mut rows = String::new();
    for k in keys {
        let t = &tiers[k];
        let fallbacks = t.fallbacks.join(", ");
        let downgrade = match &t.downgrade_to {
            Some(d) => d.as_str(),
            None => "—",
        };
        let _ = write!(
            rows,
            r##"<tr id="tier-{k}">
              <td><strong>{k}</strong></td>
              <td><form hx-post="/admin/tiers/{k}" hx-target="#tier-{k}" hx-swap="outerHTML" hx-trigger="submit" class="inline"><input name="primary" value="{primary}" /></form></td>
              <td><code>{fallbacks}</code></td>
              <td>{downgrade}</td>
              <td>{allow}</td>
            </tr>"##,
            k = html_escape(k),
            primary = html_escape(&t.primary),
            fallbacks = html_escape(&fallbacks),
            downgrade = html_escape(downgrade),
            allow = if t.allow_tier_downgrade { "yes" } else { "no" },
        );
    }
    format!(
        r##"<table>
  <thead><tr><th>Tier</th><th>Primary model</th><th>Fallbacks</th><th>Downgrade to</th><th>Allow downgrade</th></tr></thead>
  <tbody>{rows}</tbody>
</table>"##
    )
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}
