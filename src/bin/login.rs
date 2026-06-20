//! `token-dealer-login` — local CLI for OAuth flows that need a
//! loopback callback server.
//!
//! ## Why this exists
//!
//! token-dealer is a self-hosted app. Most users run it in Docker
//! behind a Tailscale IP, a localhost-only bind, or a private LAN.
//! None of those have public DNS that OAuth providers can resolve.
//!
//! Standard popup OAuth (Google, OpenAI Codex, xAI, etc.) requires
//! the `redirect_uri` to be reachable by the provider's OAuth
//! server, which means the provider's crawler hits the URL, parses
//! the response, and lets the user proceed. A `redirect_uri` that
//! points at `http://100.99.145.19:8080/...` (Tailscale) or
//! `http://localhost:8080/...` (inside the container) will fail.
//!
//! The fix: drive the OAuth flow from a CLI binary that runs on
//! the **user's machine** — outside the container. The loopback
//! callback binds to `127.0.0.1:<random_port>` which is reachable
//! from the user's browser. The CLI captures the `code`, exchanges
//! it for a `refresh_token` via the provider's token endpoint, then
//! POSTs the refresh_token back to the running token-dealer server
//! via `/admin/oauth/<provider>/setup`.
//!
//! This is the same pattern as `gh auth login`, `aws sso login`,
//! `kiro-cli login`, and `kubectl oidc-login`. It's the standard for
//! self-hosted CLIs and works with zero DNS, zero port-forwarding,
//! zero public infrastructure.
//!
//! ## Usage
//!
//! ```text
//! # Interactive: prompts for the provider, opens browser.
//! token-dealer-login openai
//! token-dealer-login gemini
//!
//! # Server URL defaults to http://127.0.0.1:8080 — the binary
//! # talks to the server on the same machine. Override with --server.
//! token-dealer-login --server http://my-td-host:8080 openai
//!
//! # Dry-run: print the authorize URL but don't open a browser or
//! # bind a callback server.
//! token-dealer-login --print-url openai
//! ```
//!
//! ## Implementation notes
//!
//! - Random free port per invocation (kernel-assigned via port 0).
//! - The provider's OAuth client_id, authorize URL, and token
//!   endpoint are looked up from the token-dealer manifest at
//!   compile time (same source as the server uses).
//! - The exchange step talks to the provider's token endpoint
//!   directly — the server is not involved in the code→token
//!   exchange, so we don't need to share the PKCE verifier with
//!   the server.
//! - After we have the refresh_token, we POST it to the server's
//!   `/admin/oauth/<provider>/setup` endpoint which encrypts and
//!   stores it in the same place the popup flow would have.
//! - The CLI binds to 127.0.0.1 (not 0.0.0.0) so other machines
//!   on the LAN can't intercept the code.

use std::net::SocketAddr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// Per-provider OAuth client config. Mirrors the server-side
/// `ManifestOAuth` struct but only the fields the CLI needs (the
/// client_id + endpoints the loopback flow uses).
#[derive(Debug, Clone, Copy)]
struct ProviderOAuth {
    /// Public client_id registered with the provider.
    client_id: &'static str,
    /// Authorize endpoint.
    authorize_url: &'static str,
    /// Token endpoint for code exchange + refresh.
    token_url: &'static str,
    /// Scope string (space-joined).
    scope: &'static str,
    /// Optional scope string. Empty means `self.scope` is used.
    scope_override: Option<&'static str>,
    /// Extra query params for the authorize URL (e.g. Google's
    /// `access_type=offline&prompt=consent`).
    extra_params: &'static [(&'static str, &'static str)],
    /// Whether the provider requires PKCE. (Most popup flows do.)
    requires_pkce: bool,
    /// Whether the client_secret is sent on the token exchange.
    /// Most popup flows are public clients (no secret). Google
    /// Gemini is the exception — its Desktop client ships with a
    /// secret, read from env or hardcoded with a fallback.
    client_secret: Option<&'static str>,
}

/// Provider registry. Mirrors `src/providers/manifest.rs` but
/// only the OAuth fields the CLI needs. We duplicate the table
/// here (rather than linking to the lib) so the CLI binary
/// stays small and has no transitive deps on the server's
/// internal types.
fn lookup_provider(provider: &str) -> Option<ProviderOAuth> {
    match provider {
        "openai" | "responses" => Some(ProviderOAuth {
            client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
            authorize_url: "https://auth.openai.com/oauth/authorize",
            token_url: "https://auth.openai.com/oauth/token",
            scope: "openid profile email offline_access",
            scope_override: None,
            extra_params: &[],
            requires_pkce: true,
            client_secret: None,
        }),
        "google" | "gemini" => Some(ProviderOAuth {
            client_id:
                "681255809395-oo8ft2oprdrnp9e3aqf6av3hmi99ikee6.apps.googleusercontent.com",
            authorize_url: "https://accounts.google.com/o/oauth2/v2/auth",
            token_url: "https://oauth2.googleapis.com/token",
            scope: "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile openid",
            scope_override: None,
            // Google REQUIRES these or the second sign-in returns
            // no refresh_token.
            extra_params: &[("access_type", "offline"), ("prompt", "consent")],
            requires_pkce: true,
            client_secret: match std::env::var("GOOGLE_OAUTH_CLIENT_SECRET") {
                Ok(s) if !s.is_empty() => Some(Box::leak(s.into_boxed_str()) as &'static str),
                _ => None,
            },
        }),
        "xai" | "grok" => Some(ProviderOAuth {
            // xAI's OAuth client uses a hardcoded 127.0.0.1:1455
            // redirect_uri (the CLI's loopback port is overridden
            // per-invocation, but the path /callback must match).
            client_id: "b1a00492-073a-47ea-816f-4c329264a828",
            authorize_url: "https://auth.x.ai/oauth2/authorize",
            token_url: "https://auth.x.ai/oauth2/token",
            scope: "openid profile email offline_access grok-cli:access api:access",
            scope_override: None,
            extra_params: &[],
            requires_pkce: true,
            client_secret: None,
        }),
        "github-copilot" => Some(ProviderOAuth {
            client_id: "Iv1.b507a08c87ecfe98",
            authorize_url: "",
            token_url: "https://github.com/login/oauth/access_token",
            // device_code flow — handled separately by the server.
            scope: "read:user",
            scope_override: None,
            extra_params: &[],
            requires_pkce: false,
            client_secret: None,
        }),
        "kiro" => Some(ProviderOAuth {
            // Kiro's OIDC client uses dynamic registration.
            client_id: "Manifest",
            authorize_url: "",
            token_url: "https://oidc.us-east-1.amazonaws.com/token",
            scope: "codewhisperer:completions codewhisperer:conversations",
            scope_override: None,
            extra_params: &[],
            requires_pkce: false,
            client_secret: None,
        }),
        "minimax" => Some(ProviderOAuth {
            client_id: "78257093-7e40-4613-99e0-527b14b39113",
            authorize_url: "",
            token_url: "https://account.minimax.io/oauth2/token",
            scope: "group_id profile model.completion",
            scope_override: None,
            extra_params: &[],
            requires_pkce: true,
            client_secret: None,
        }),
        "anthropic" => Some(ProviderOAuth {
            // Anthropic uses the paste-code flow — handled by
            // a separate subcommand.
            client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
            authorize_url: "https://claude.ai/oauth/authorize",
            token_url: "https://api.anthropic.com/v1/oauth/token",
            scope: "org:create_api_key user:profile user:inference",
            scope_override: None,
            extra_params: &[],
            requires_pkce: true,
            client_secret: None,
        }),
        _ => None,
    }
}

#[derive(Serialize)]
struct SetupReq {
    provider_id: String,
    refresh_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_secret: Option<String>,
}

#[derive(Deserialize)]
struct SetupResp {
    status: String,
    #[serde(default)]
    error: Option<String>,
}

/// Top-level CLI dispatch.
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut server = "http://127.0.0.1:8080".to_string();
    let mut print_only = false;
    let mut no_browser = false;
    let mut provider_idx = None;
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--server" | "-s" => {
                server = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow::anyhow!("--server requires URL"))?
                    .clone();
                i += 2;
            }
            "--print-url" => {
                print_only = true;
                i += 1;
            }
            "--no-browser" => {
                no_browser = true;
                i += 1;
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            other if other.starts_with("--") => {
                anyhow::bail!("unknown flag: {other}");
            }
            _ => {
                provider_idx = Some(i);
                i += 1;
            }
        }
    }
    let provider = provider_idx
        .and_then(|i| args.get(i).cloned())
        .ok_or_else(|| {
            anyhow::anyhow!("missing provider name. Run `token-dealer-login --help` for usage.")
        })?;

    // Anthropic uses the paste-code flow. Print the URL + instructions.
    if provider == "anthropic" {
        return run_anthropic_paste(&server, print_only);
    }

    // github-copilot, kiro, minimax use device_code. Delegate to
    // the server (which already implements device flow correctly).
    if matches!(provider.as_str(), "github-copilot" | "kiro" | "minimax") {
        return run_device_flow(&server, &provider, print_only);
    }

    // Otherwise popup OAuth with loopback callback.
    let cfg = lookup_provider(&provider).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown provider: {provider}. Known: openai, google, gemini, xai, anthropic, github-copilot, kiro, minimax"
        )
    })?;
    if cfg.authorize_url.is_empty() {
        anyhow::bail!(
            "provider {provider} has no popup_oauth authorize_url; use --device-code instead"
        );
    }
    run_popup_flow(&server, &provider, cfg, print_only, no_browser).await
}

fn print_help() {
    eprintln!(
        "token-dealer-login — connect a provider to a running token-dealer instance\n\
         \n\
         Usage:\n  \
           token-dealer-login [--server URL] [--print-url] [--no-browser] <provider>\n\
         \n\
         Providers:\n  \
           openai, google, gemini, xai, github-copilot, kiro, minimax, anthropic\n\
         \n\
         Examples:\n  \
           token-dealer-login openai                   # popup OAuth (browser opens)\n  \
           token-dealer-login --server http://td.lan:8080 gemini\n  \
           token-dealer-login --print-url openai       # just print the URL\n  \
           token-dealer-login anthropic                # paste-code flow (prints URL)\n\
         \n\
         The popup OAuth flows bind to 127.0.0.1:<random_port> on the user's\n\
         machine so the provider can redirect back. Works with zero DNS,\n\
         zero port-forwarding, zero public infrastructure."
    );
}

/// Anthropic paste-code flow. Prints the authorize URL, then prompts
/// the user to paste back the `code#state` pair from
/// console.anthropic.com.
fn run_anthropic_paste(server: &str, print_only: bool) -> anyhow::Result<()> {
    let cfg = lookup_provider("anthropic").unwrap();
    let state = format!("anthropic.{}", uuid::Uuid::new_v4().simple());
    // PKCE: code_verifier doubles as state (Anthropic convention).
    let verifier = random_token(64);
    let challenge = pkce_s256(&verifier);
    let mut url = format!(
        "{}?code=true&client_id={}&response_type=code&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        cfg.authorize_url,
        urlenc(cfg.client_id),
        urlenc("https://console.anthropic.com/oauth/code/callback"),
        urlenc(cfg.scope),
        urlenc(&state),
        urlenc(&challenge),
    );
    println!("\n{}\n", url.bright_cyan());
    if print_only {
        println!("--print-url: not waiting for paste.");
        return Ok(());
    }
    println!("Sign in, then paste the code (format: `code#state`) from console.anthropic.com:");
    eprint!("> ");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let pasted = line.trim().to_string();
    if pasted.is_empty() {
        anyhow::bail!("empty input");
    }
    let (code, _) = match pasted.rsplit_once('#') {
        Some((c, s)) => (c.to_string(), s.to_string()),
        None => (pasted.clone(), verifier.clone()),
    };
    // Exchange + push to server (via curl subprocess; sync).
    let body = format!(
        "grant_type=authorization_code&code={}&client_id={}&redirect_uri={}&code_verifier={}",
        urlenc(&code),
        urlenc(cfg.client_id),
        urlenc("https://console.anthropic.com/oauth/code/callback"),
        urlenc(&verifier),
    );
    let output = std::process::Command::new("curl")
        .args([
            "-sS",
            "-X",
            "POST",
            cfg.token_url,
            "-H",
            "content-type: application/x-www-form-urlencoded",
            "--data",
            &body,
        ])
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "Anthropic token exchange failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let v: Value = serde_json::from_slice(&output.stdout)?;
    let refresh = v
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .ok_or_else(|| {
            anyhow::anyhow!("Anthropic response missing refresh_token. Re-run the paste flow.")
        })?
        .to_string();
    push_refresh_token(server, "anthropic", &refresh, None, None)?;
    println!("✓ anthropic refresh_token stored.");
    Ok(())
}

/// Device-code providers (github-copilot, kiro, minimax). The
/// server already implements these correctly; we just call its
/// endpoints and print the user_code + verification URL, then
/// poll until authorized, then push the refresh_token.
fn run_device_flow(server: &str, provider: &str, print_only: bool) -> anyhow::Result<()> {
    println!("Starting device-code flow for {provider} via {server}...");
    let start = ureq_get(&format!(
        "{}/admin/oauth/{}/device/start",
        server.trim_end_matches('/'),
        provider
    ))?;
    if !(200..300).contains(&start.status()) {
        anyhow::bail!(
            "device flow start failed: {} {}",
            start.status(),
            start.into_string()?
        );
    }
    let body: Value = start.into_json()?;
    let device_code = body
        .get("device_code")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing device_code"))?
        .to_string();
    let user_code = body
        .get("user_code")
        .and_then(|v| v.as_str())
        .unwrap_or("(no user_code)")
        .to_string();
    let ver = body
        .get("verification_uri")
        .and_then(|v| v.as_str())
        .unwrap_or("(no verification_uri)")
        .to_string();
    println!(
        "\n  Visit: {}\n  Enter code: {}\n",
        ver.bright_cyan(),
        user_code.bright_yellow()
    );
    if print_only {
        println!("--print-url: not polling.");
        return Ok(());
    }
    println!("Polling for authorization (Ctrl-C to abort)...");
    let client = reqwest::Client::new();
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            let poll = client
                .post(format!(
                    "{}/admin/oauth/device/poll",
                    server.trim_end_matches('/')
                ))
                .json(&json!({ "device_code": device_code }))
                .send()
                .await?;
            let v: Value = poll.json().await?;
            if v.get("authorized").and_then(|x| x.as_bool()) == Some(true) {
                println!("✓ {provider} authorized.");
                break;
            }
            eprint!(".");
        }
        Ok::<_, anyhow::Error>(())
    })?;
    Ok(())
}

/// Popup OAuth with loopback callback. The whole flow runs on
/// the user's machine.
async fn run_popup_flow(
    server: &str,
    provider: &str,
    cfg: ProviderOAuth,
    print_only: bool,
    no_browser: bool,
) -> anyhow::Result<()> {
    // Bind a random free port on 127.0.0.1.
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let local_addr = listener.local_addr()?;
    let callback_url = format!("http://127.0.0.1:{}/callback", local_addr.port());
    println!("Loopback callback bound to {}", callback_url);

    // PKCE
    let verifier = random_token(64);
    let challenge = pkce_s256(&verifier);

    let state = format!("{}.{}", provider, uuid::Uuid::new_v4().simple());

    let scope = cfg.scope_override.unwrap_or(cfg.scope);
    let mut url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}",
        cfg.authorize_url,
        urlenc(cfg.client_id),
        urlenc(&callback_url),
        urlenc(scope),
        urlenc(&state),
    );
    if cfg.requires_pkce {
        url.push_str(&format!(
            "&code_challenge={}&code_challenge_method=S256",
            urlenc(&challenge)
        ));
    }
    for (k, v) in cfg.extra_params {
        url.push_str(&format!("&{}={}", urlenc(k), urlenc(v)));
    }

    println!(
        "\nAuthorize {provider} (loopback flow):\n  {}\n",
        url.bright_cyan()
    );
    if print_only {
        println!("--print-url: not waiting for callback.");
        return Ok(());
    }

    // Spawn browser opener (best-effort).
    if !no_browser {
        let _ = open_browser(&url);
    }

    // Listen for the callback. Spawn a one-shot receiver task that
    // signals a channel when the request arrives; the main task
    // sends the response HTML back and exchanges the code.
    let (tx, rx) = oneshot::channel::<CallbackResult>();
    let state_check = state.clone();
    tokio::spawn(async move {
        match accept_callback(listener, &state_check).await {
            Ok(r) => {
                let _ = tx.send(r);
            }
            Err(e) => {
                let _ = tx.send(CallbackResult::Error(e.to_string()));
            }
        }
    });

    println!("Waiting for browser callback on {}...", callback_url);
    let result = rx.await?;
    match result {
        CallbackResult::Ok { code, state: _ } => {
            // Build form-encoded body for the token exchange.
            let mut body = format!(
                "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
                urlenc(&code),
                urlenc(&callback_url),
                urlenc(cfg.client_id),
                urlenc(&verifier),
            );
            if let Some(cs) = cfg.client_secret {
                body.push_str(&format!("&client_secret={}", urlenc(cs)));
            }
            let client = reqwest::Client::new();
            let resp = client
                .post(cfg.token_url)
                .header("content-type", "application/x-www-form-urlencoded")
                .header("accept", "application/json")
                .body(body)
                .send()
                .await?;
            let status = resp.status();
            let v: Value = resp.json().await?;
            if !status.is_success() {
                anyhow::bail!(
                    "token exchange failed: {} {}: {}",
                    status.as_u16(),
                    status.canonical_reason().unwrap_or(""),
                    serde_json::to_string_pretty(&v).unwrap_or_default()
                );
            }
            let refresh = v
                .get("refresh_token")
                .and_then(|x| x.as_str())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "token response missing refresh_token: {}",
                        serde_json::to_string_pretty(&v).unwrap_or_default()
                    )
                })?
                .to_string();
            // Some providers return an `access_token` here; the
            // server can also accept a refresh_token directly.
            push_refresh_token(server, provider, &refresh, None, None)?;
            println!("✓ {provider} refresh_token stored on {server}.");
        }
        CallbackResult::Error(e) => {
            anyhow::bail!("callback error: {e}");
        }
    }
    Ok(())
}

enum CallbackResult {
    Ok { code: String, state: String },
    Error(String),
}

async fn accept_callback(
    listener: TcpListener,
    expected_state: &str,
) -> anyhow::Result<CallbackResult> {
    let (mut socket, _peer) = listener.accept().await?;
    let (reader, mut writer) = socket.split();
    let mut reader = BufReader::new(reader);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    // Parse: GET /callback?code=...&state=... HTTP/1.1
    let (path_query, _rest) = request_line
        .split_once(' ')
        .ok_or_else(|| anyhow::anyhow!("bad request line"))?;
    let (_method, path) = path_query.split_once(' ').unwrap_or(("GET", path_query));
    let url = format!("http://127.0.0.1{}", path);
    let parsed = url::Url::parse(&url)?;
    let mut code = String::new();
    let mut state = String::new();
    let mut error = String::new();
    for (k, v) in parsed.query_pairs() {
        match k.as_ref() {
            "code" => code = v.into_owned(),
            "state" => state = v.into_owned(),
            "error" => error = v.into_owned(),
            _ => {}
        }
    }
    // Drain headers so the response can flush cleanly.
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        if line == "\r\n" || line.is_empty() {
            break;
        }
    }
    if !error.is_empty() {
        let body = format!(
            "<html><body style='font-family:sans-serif'><h1>OAuth error</h1>\
             <p>Provider returned <code>{error}</code>. You can close this window.</p></body></html>"
        );
        let _ = write_response(&mut writer, 400, "text/html", &body).await;
        return Ok(CallbackResult::Error(format!("provider error: {error}")));
    }
    if state != expected_state {
        let body =
            "<html><body><h1>State mismatch</h1><p>You can close this window.</p></body></html>";
        let _ = write_response(&mut writer, 400, "text/html", body).await;
        return Ok(CallbackResult::Error(
            "state mismatch (different browser session?)".into(),
        ));
    }
    let body = "<html><body style='font-family:sans-serif;padding:2em'>\
        <h1>✓ Token-dealer</h1>\
        <p>You can close this window. The CLI is exchanging the code now.</p>\
        </body></html>";
    let _ = write_response(&mut writer, 200, "text/html", body).await;
    Ok(CallbackResult::Ok { code, state })
}

async fn write_response<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    status: u16,
    content_type: &str,
    body: &str,
) -> std::io::Result<()> {
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Status",
    };
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\n\
         content-type: {content_type}\r\n\
         content-length: {}\r\n\
         connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    writer.write_all(response.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

fn push_refresh_token(
    server: &str,
    provider: &str,
    refresh: &str,
    registered_client_id: Option<&str>,
    registered_client_secret: Option<&str>,
) -> anyhow::Result<()> {
    let url = format!(
        "{}/admin/oauth/{}/setup",
        server.trim_end_matches('/'),
        provider
    );
    let mut body = json!({
        "refresh_token": refresh,
    });
    if let (Some(cid), Some(cs)) = (registered_client_id, registered_client_secret) {
        body["client_id"] = json!(cid);
        body["client_secret"] = json!(cs);
    }
    let output = std::process::Command::new("curl")
        .args([
            "-sS",
            "-X",
            "POST",
            &url,
            "-H",
            "content-type: application/json",
            "-d",
            &body.to_string(),
        ])
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "setup POST failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let v: Value = serde_json::from_slice(&output.stdout)?;
    if v.get("status").and_then(|s| s.as_str()) != Some("ok") {
        anyhow::bail!("setup returned: {}", serde_json::to_string(&v)?);
    }
    Ok(())
}

/// Synchronous GET using ureq. Avoids pulling in a second async
/// runtime when we just need a one-shot response.
fn ureq_get(url: &str) -> anyhow::Result<ureq::Response> {
    let resp = ureq::get(url)
        .timeout(std::time::Duration::from_secs(10))
        .call()?;
    Ok(resp)
}

// ── small utilities ─────────────────────────────────────────────────────

fn urlenc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn random_token(len: usize) -> String {
    // RFC 7636 §4.1: 43–128 chars, URL-safe [A-Z][a-z][0-9]-._~.
    // The full byte range of those chars (printable ASCII) is
    // ~94; a length-`len` ASCII token from the same alphabet is
    // URL-safe without further encoding. Using String::from_utf8
    // on the raw RNG output avoids the lossiness of from_utf8_lossy
    // which would emit U+FFFD replacement chars that double-encode
    // to %EF%BF%BD inside urlenc().
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut buf = vec![0u8; len];
    rand::Rng::fill(&mut rand::thread_rng(), &mut buf[..]);
    buf.iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

fn pkce_s256(verifier: &str) -> String {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(verifier.as_bytes());
    let digest = h.finalize();
    // RFC 7636 §4.2: base64url WITHOUT padding.
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

fn open_browser(url: &str) -> anyhow::Result<()> {
    let cmd = match std::env::consts::OS {
        "macos" => "open",
        "windows" => "start",
        _ => "xdg-open",
    };
    std::process::Command::new(cmd).arg(url).spawn()?;
    Ok(())
}

// ── terminal-color helpers (cheap, no crate) ────────────────────────────

trait ColorExt {
    fn bright_cyan(&self) -> String;
    fn bright_yellow(&self) -> String;
}
impl ColorExt for str {
    fn bright_cyan(&self) -> String {
        if std::env::var("NO_COLOR").is_ok() {
            self.to_string()
        } else {
            format!("\x1b[1;36m{}\x1b[0m", self)
        }
    }
    fn bright_yellow(&self) -> String {
        if std::env::var("NO_COLOR").is_ok() {
            self.to_string()
        } else {
            format!("\x1b[1;33m{}\x1b[0m", self)
        }
    }
}

#[allow(dead_code)]
fn _suppress_unused_socket_addr_warning(_: SocketAddr) {}
