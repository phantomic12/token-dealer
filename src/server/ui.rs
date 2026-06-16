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

/// Render a ProviderType as its kebab-case string form (e.g.
/// `ProviderType::GithubCopilot` → `"github-copilot"`). Used by the
/// UI to match incoming `?type=...` query params against the manifest.
pub fn provider_type_to_str(pt: &crate::config::types::ProviderType) -> &'static str {
    use crate::config::types::ProviderType;
    match pt {
        ProviderType::Anthropic => "anthropic",
        ProviderType::Google => "google",
        ProviderType::Kiro => "kiro",
        ProviderType::Responses => "responses",
        ProviderType::Generic => "generic",
        ProviderType::Openai => "openai",
        ProviderType::Openrouter => "openrouter",
        ProviderType::Tokenrouter => "tokenrouter",
        ProviderType::Groq => "groq",
        ProviderType::Deepseek => "deepseek",
        ProviderType::Fireworks => "fireworks",
        ProviderType::Mistral => "mistral",
        ProviderType::Xai => "xai",
        ProviderType::Qwen => "qwen",
        ProviderType::Moonshot => "moonshot",
        ProviderType::Zai => "zai",
        ProviderType::Xiaomi => "xiaomi",
        ProviderType::Minimax => "minimax",
        ProviderType::Byteplus => "byteplus",
        ProviderType::Nvidia => "nvidia",
        ProviderType::OpencodeGo => "opencode-go",
        ProviderType::OpencodeZen => "opencode-zen",
        ProviderType::Kilo => "kilo",
        ProviderType::Commandcode => "commandcode",
        ProviderType::GithubCopilot => "github-copilot",
        ProviderType::Gitlawb => "gitlawb",
        ProviderType::Ollama => "ollama",
        ProviderType::OllamaCloud => "ollama-cloud",
        ProviderType::LlamaCpp => "llamacpp",
        ProviderType::LmStudio => "lmstudio",
    }
}

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
.wizard-steps { display: flex; gap: 6px; margin-bottom: 16px; font-size: 12px;
  color: var(--text-dim); }
.wizard-steps .step { padding: 4px 10px; border-radius: 4px;
  background: var(--bg-elev); }
.wizard-steps .step.active { background: var(--accent); color: #0d1117;
  font-weight: 600; }
.provider-grid { display: grid; grid-template-columns: repeat(4, 1fr);
  gap: 10px; margin-top: 12px; }
.provider-card { display: block; padding: 12px; border: 1px solid var(--border);
  border-radius: 6px; background: var(--bg-elev); text-decoration: none;
  color: var(--text); cursor: pointer; transition: border-color 0.1s,
  transform 0.05s; }
.provider-card:hover { border-color: var(--accent); transform: translateY(-1px); }
.provider-card.local { border-color: var(--yellow); }
.provider-card .name { font-weight: 600; font-size: 13px; }
.provider-card .meta { font-size: 11px; color: var(--text-dim);
  margin-top: 4px; font-family: var(--mono); }
.provider-card .badge { margin-top: 6px; }
.wizard-panel { border: 1px solid var(--border); border-radius: 6px;
  padding: 20px; background: var(--bg-elev); margin-top: 12px; }
.wizard-panel h2 { margin-top: 0; }
.test-result { padding: 10px 14px; border-radius: 5px; margin-top: 12px;
  font-size: 13px; font-family: var(--mono); }
.test-result.ok { background: rgba(63, 185, 80, 0.15); color: var(--green);
  border: 1px solid rgba(63, 185, 80, 0.3); }
.test-result.error { background: rgba(248, 81, 73, 0.15); color: var(--red);
  border: 1px solid rgba(248, 81, 73, 0.3); }
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
    {nav_rules}
    {nav_logs}
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
        nav_logs = nav("/ui/logs", "Logs", "logs"),
        nav_rules = nav("/ui/rules", "Rules", "rules"),
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
<p class="dim">Adapters wired in to handle <code>provider/model</code> requests. The list and each adapter's defaults come from the manifest table.</p>
<div class="notice">Changes you make here are live in memory immediately and persisted to <code class="kbd">{path}</code>. Click "Save to disk" in the top right to force a flush.</div>

<a class="wizard-cta" href="/ui/providers/new">+ Add a provider</a>

<h2>Configured providers</h2>
{list}
"##,
        path = state.config.path().display(),
        list = render_providers_list(&snap.providers).await,
    );

    Html(layout("providers", "Providers", &body, None)).into_response()
}

/// Step 1 of the add-provider wizard: a grid of provider cards.
/// Clicking a card advances to step 2 with the manifest defaults
/// pre-filled in the form.
pub async fn providers_new_step1(State(_state): State<AppState>) -> Response {
    let body = render_wizard_step1();
    Html(layout("providers", "Providers", &body, None)).into_response()
}

/// Step 2 of the wizard: a form pre-filled from the manifest.
/// The user enters an API key (or env-var reference), then clicks
/// "Test connection" to verify, then "Save" to persist.
pub async fn providers_new_step2(
    State(_state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let provider_type = params
        .get("type")
        .cloned()
        .unwrap_or_else(|| "openai".to_string());
    let body = render_wizard_step2(&provider_type);
    Html(layout("providers", "Providers", &body, None)).into_response()
}

fn render_wizard_step1() -> String {
    use crate::providers::manifest;
    // Friendly display name + tier grouping for the picker. The
    // order is curated: most common first, then alphabetical.
    let order: &[(&str, &str)] = &[
        ("anthropic", "Anthropic (Claude)"),
        ("openai", "OpenAI"),
        ("google", "Google (Gemini)"),
        ("kiro", "Kiro (AWS)"),
        ("responses", "OpenAI Responses (o3)"),
        ("generic", "Generic OpenAI-compatible"),
        ("openrouter", "OpenRouter"),
        ("tokenrouter", "TokenRouter"),
        ("groq", "Groq"),
        ("deepseek", "DeepSeek"),
        ("fireworks", "Fireworks"),
        ("mistral", "Mistral"),
        ("xai", "xAI (Grok)"),
        ("qwen", "Qwen (Alibaba)"),
        ("moonshot", "Moonshot (Kimi)"),
        ("zai", "Z.ai (GLM)"),
        ("xiaomi", "Xiaomi (MiMo)"),
        ("minimax", "Minimax"),
        ("byteplus", "BytePlus"),
        ("nvidia", "NVIDIA NIM"),
        ("opencode-go", "OpenCode Go"),
        ("opencode-zen", "OpenCode Zen"),
        ("kilo", "Kilo"),
        ("commandcode", "CommandCode"),
        ("github-copilot", "GitHub Copilot"),
        ("gitlawb", "Gitlawb"),
        ("ollama", "Ollama (local)"),
        ("ollama-cloud", "Ollama Cloud"),
        ("llamacpp", "llama.cpp (local)"),
        ("lmstudio", "LM Studio (local)"),
    ];

    let mut cards = String::new();
    for (type_str, display) in order {
        let target = type_str.to_string();
        let info = crate::providers::manifest::ALL_TYPES
            .iter()
            .find(|pt| provider_type_to_str(pt) == target.as_str())
            .and_then(|pt| manifest::lookup(*pt));
        let (badge, badge_class, meta) = match info {
            Some(m) => {
                let badge = if m.local_only { "local" } else { "cloud" };
                let class = if m.local_only { "local" } else { "healthy" };
                let needs_key = m.requires_key;
                let meta = if needs_key {
                    format!("requires API key · {}", m.base_url)
                } else {
                    format!("no key · {}", m.base_url)
                };
                (badge.to_string(), class.to_string(), meta)
            }
            None => (
                "?".to_string(),
                "healthy".to_string(),
                "no manifest".to_string(),
            ),
        };
        let _ = write!(
            cards,
            r##"<a class="provider-card {cls}" href="/ui/providers/new/config?type={t}" hx-get="/ui/providers/new/config?type={t}" hx-target="#wizard" hx-swap="outerHTML" hx-push-url="true">
              <div class="name">{display}</div>
              <div class="meta">{meta}</div>
              <span class="badge {cls}">{badge}</span>
            </a>"##,
            cls = badge_class,
            t = type_str,
            display = display,
            meta = meta,
            badge = badge,
        );
    }

    format!(
        r##"
<h1>Add a provider</h1>
<div class="wizard-steps">
  <span class="step active">1. Pick provider</span>
  <span class="step">2. Configure</span>
  <span class="step">3. Test + save</span>
</div>
<p class="dim">Click a provider to continue. Defaults are filled in from the manifest table — you usually only need to add the API key.</p>

<div id="wizard">
  <div class="provider-grid">{cards}</div>
</div>

<p style="margin-top: 24px;"><a href="/ui/providers">Cancel</a></p>
"##
    )
}

fn render_wizard_step2(provider_type: &str) -> String {
    use crate::config::types::ProviderType;
    use crate::providers::manifest;

    let pt = crate::providers::manifest::ALL_TYPES
        .iter()
        .find(|p| provider_type_to_str(p) == provider_type)
        .copied()
        .unwrap_or(ProviderType::Generic);
    let info = manifest::lookup(pt);
    let (default_url, default_model, default_path, requires_key, local_only) = match info {
        Some(m) => (
            m.base_url.to_string(),
            m.default_model.to_string(),
            m.path.to_string(),
            m.requires_key,
            m.local_only,
        ),
        None => (
            "https://api.example.com".to_string(),
            "default".to_string(),
            "/v1/chat/completions".to_string(),
            true,
            false,
        ),
    };

    let id_suggestion = if provider_type == "generic" {
        "my-proxy".to_string()
    } else {
        provider_type.to_string()
    };

    let key_label = if local_only {
        "Key (any value, e.g. &quot;ollama&quot; — not validated)"
    } else if requires_key {
        "API key (or <code>$&#123;ENV_VAR&#125;</code> reference)"
    } else {
        "API key (optional)"
    };

    let display_name = match provider_type {
        "anthropic" => "Anthropic (Claude)",
        "openai" => "OpenAI",
        "google" => "Google (Gemini)",
        "kiro" => "Kiro (AWS)",
        "responses" => "OpenAI Responses (o3)",
        "generic" => "Generic OpenAI-compatible",
        t => t,
    };

    format!(
        r##"
<h1>Add a provider</h1>
<div class="wizard-steps">
  <span class="step"><a href="/ui/providers/new" style="color:inherit;text-decoration:none;">1. Pick provider</a></span>
  <span class="step active">2. Configure</span>
  <span class="step">3. Test + save</span>
</div>

<div id="wizard" class="wizard-panel">
  <h2>{display_name} <span class="badge {local_badge}">{local_label}</span></h2>
  <p class="dim">Defaults are pre-filled. Override base URL or path for self-hosted proxies / staging. The test call will hit <code>{default_path}</code> with the configured key.</p>

  <form id="provider-form"
        hx-post="/admin/providers"
        hx-target="#wizard"
        hx-swap="outerHTML"
        hx-on::after-request="document.getElementById('wizard').innerHTML = '<div class=&quot;flash success&quot;>Provider saved. Reloading…</div>'; setTimeout(() => location.reload(), 600)">
    <input type="hidden" name="type" value="{pt_as_str}" />
    <div class="row three">
      <div>
        <label>ID (used in <code>model</code> field)</label>
        <input name="id" value="{id_suggestion}" required />
      </div>
      <div>
        <label>{key_label}</label>
        <input name="key" placeholder="${{ANTHROPIC_API_KEY}}" autofocus />
      </div>
      <div>
        <label>Default model</label>
        <input name="default_model" value="{default_model}" />
      </div>
    </div>
    <div class="row">
      <div>
        <label>Base URL (optional — defaults from manifest)</label>
        <input name="base_url" value="{default_url}" />
      </div>
      <div>
        <label>Path (optional)</label>
        <input name="path" value="{default_path}" />
      </div>
    </div>

    <div id="test-result"></div>

    <div class="actions">
      <button type="button" class="secondary"
              hx-post="/admin/providers/test"
              hx-include="#provider-form"
              hx-target="#test-result"
              hx-swap="innerHTML">
        Test connection
      </button>
      <button type="submit">Save provider</button>
      <span class="muted">Test runs against the upstream with the key above. No save until you click Save.</span>
    </div>
  </form>
</div>

<p style="margin-top: 16px;"><a href="/ui/providers/new">← back to provider picker</a></p>
"##,
        display_name = display_name,
        local_badge = if local_only { "local" } else { "healthy" },
        local_label = if local_only { "local" } else { "cloud" },
        default_path = default_path,
        pt_as_str = provider_type_to_str(&pt),
        id_suggestion = id_suggestion,
        default_model = default_model,
        default_url = default_url,
    )
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
        r#"
<h1>Tiers</h1>
<p class="dim">Per-tier primary model + fallbacks. Requests with <code>model: "tier"</code> or <code>x-router-tier: tier</code> route to that tier's primary; the first model in <code>fallbacks</code> is tried if the primary fails.</p>
<div class="notice">Edit the primary inline, click Save. "Save to disk" in the top right flushes to <code class="kbd">{path}</code>.</div>

{list}
"#,
        path = state.config.path().display(),
        list = render_tiers_list(&snap.tiers),
    );

    Html(layout("tiers", "Tiers", &body, None)).into_response()
}

pub async fn rules_page(State(state): State<AppState>) -> Response {
    let snap = state.config.snapshot().await;
    let body = format!(
        r##"
<h1>Rules</h1>
<p class="dim">Detection rules. Evaluated in order; the first match (or the highest tier floor) wins. Useful for forcing specific request shapes to specific tiers.</p>
<div class="notice">Changes persist to <code class="kbd">{path}</code> immediately.</div>

<h2>Add a rule</h2>
<form hx-post="/admin/rules" hx-target="#rule-list" hx-swap="outerHTML" hx-on::after-request="this.reset()">
  <div class="row three">
    <div>
      <label>has_tools = true/false (optional)</label>
      <select name="has_tools">
        <option value="">(any)</option>
        <option value="true">true</option>
        <option value="false">false</option>
      </select>
    </div>
    <div>
      <label>input_tokens &gt; N (optional)</label>
      <input name="input_tokens_gt" type="number" placeholder="50000" />
    </div>
    <div>
      <label>force tier</label>
      <select name="tier">
        <option value="simple">simple</option>
        <option value="standard" selected>standard</option>
        <option value="complex">complex</option>
        <option value="reasoning">reasoning</option>
        <option value="high_context">high_context</option>
        <option value="multimodal">multimodal</option>
      </select>
    </div>
  </div>
  <div class="row">
    <div>
      <label>prompt contains (comma-separated keywords, optional)</label>
      <input name="prompt_contains" placeholder="reason step by step, formally prove" />
    </div>
  </div>
  <div class="actions">
    <button type="submit">Add rule</button>
    <span class="muted">Multiple conditions are AND-ed together. Empty conditions match everything.</span>
  </div>
</form>

<h2>Configured rules</h2>
{list}
"##,
        path = state.config.path().display(),
        list = render_rules_list(&snap.detection.rules),
    );

    Html(layout("rules", "Rules", &body, None)).into_response()
}

fn render_rules_list(
    rules: &[crate::config::types::DetectionRule],
) -> String {
    if rules.is_empty() {
        return r#"<div class="center-empty" id="rule-list">No rules. Add one above.</div>"#.to_string();
    }
    let mut rows = String::new();
    for (i, r) in rules.iter().enumerate() {
        let cond = render_condition(&r.condition);
        let _ = write!(
            rows,
            r##"<tr id="rule-{i}">
              <td><code>{i}</code></td>
              <td><code>{cond}</code></td>
              <td><span class="badge healthy">{tier}</span></td>
              <td><form hx-post="/admin/rules/{i}" hx-target="#rule-list" hx-swap="outerHTML" hx-confirm="Delete rule {i}?" class="inline"><button class="danger" type="submit">Delete</button></form></td>
            </tr>"##,
            i = i,
            cond = html_escape(&cond),
            tier = html_escape(&r.tier),
        );
    }
    format!(
        r##"<table id="rule-list">
  <thead><tr><th>#</th><th>Condition</th><th>Tier</th><th></th></tr></thead>
  <tbody>{rows}</tbody>
</table>"##
    )
}

fn render_condition(cond: &crate::config::types::DetectionCondition) -> String {
    let mut parts = Vec::new();
    if let Some(t) = cond.has_tools {
        parts.push(format!("has_tools = {t}"));
    }
    if let Some(n) = cond.input_tokens_gt {
        parts.push(format!("input_tokens > {n}"));
    }
    if let Some(kws) = &cond.prompt_contains {
        if !kws.is_empty() {
            parts.push(format!("prompt_contains: {}", kws.join(", ")));
        }
    }
    if parts.is_empty() {
        "(always)".to_string()
    } else {
        parts.join(" AND ")
    }
}

pub async fn logs_page(State(state): State<AppState>) -> Response {
    let filter = crate::db::queries::LogFilter {
        limit: 100,
        ..Default::default()
    };
    let rows = match state
        .db
        .with(move |conn| {
            crate::db::queries::list_requests(conn, &filter)
                .map_err(|e| anyhow::anyhow!("list logs failed: {e}"))
        })
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return Html(layout(
                "logs",
                "Logs",
                &format!(r#"<div class="flash error">DB error: {}</div>"#, e),
                None,
            ))
            .into_response();
        }
    };

    let body = format!(
        r#"
<h1>Logs</h1>
<p class="dim">Most recent {n} requests. For older data, query the SQLite file directly: <code class="kbd">sqlite3 /data/router.db</code></p>

{rows_html}
"#,
        n = rows.len(),
        rows_html = render_logs_rows(&rows),
    );

    Html(layout("logs", "Logs", &body, None)).into_response()
}

fn render_logs_rows(rows: &[crate::db::queries::RequestRow]) -> String {
    if rows.is_empty() {
        return r#"<div class="center-empty">No requests logged yet. Make a chat completion to populate this view.</div>"#.to_string();
    }
    let mut out = String::new();
    for r in rows {
        let cost = r
            .cost_usd
            .map(|c| format!("${:.5}", c))
            .unwrap_or_else(|| "—".to_string());
        let _ = write!(
            out,
            r#"<tr>
              <td><code class="kbd">{ts}</code></td>
              <td><span class="badge healthy">{tier}</span></td>
              <td><code>{provider}/{model}</code></td>
              <td>{in_tok} → {out_tok}</td>
              <td>{cost}</td>
              <td>{latency}ms</td>
              <td>{fallback_count}</td>
              <td>{finish}</td>
            </tr>"#,
            ts = html_escape(&r.created_at),
            tier = html_escape(&r.tier),
            provider = html_escape(&r.routed_provider),
            model = html_escape(&r.routed_model),
            in_tok = r.input_tokens.unwrap_or(0),
            out_tok = r.output_tokens.unwrap_or(0),
            cost = cost,
            latency = r.total_latency_ms,
            fallback_count = r.fallback_count,
            finish = html_escape(r.finish_reason.as_deref().unwrap_or("—")),
        );
    }
    format!(
        r#"<table>
  <thead><tr><th>Time</th><th>Tier</th><th>Route</th><th>Tokens (in→out)</th><th>Cost</th><th>Latency</th><th>Fallbacks</th><th>Finish</th></tr></thead>
  <tbody>{out}</tbody>
</table>"#
    )
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
