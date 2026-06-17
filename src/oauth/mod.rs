//! OAuth token store + refresh manager. Generic over the standard
//! OAuth2 `grant_type=refresh_token` flow so any provider that follows
//! the spec (Kiro social auth, ChatGPT Codex, GitHub Copilot) can
//! plug in. Credentials are stored encrypted in SQLite.
//!
//! Usage:
//!   1. POST /admin/oauth/:provider_id with the initial refresh_token
//!   2. Background task refreshes before expiry
//!   3. Adapter calls `OAuthManager::access_token(provider_id)` to
//!      get a fresh token

use crate::auth::KeyStore;
use crate::db::Db;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// Per-provider OAuth configuration: the refresh endpoint + client
/// credentials. The actual refresh is a standard OAuth2 token
/// endpoint: POST with `grant_type=refresh_token`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthConfig {
    pub provider_id: String,
    pub token_url: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    /// Optional extra body fields (e.g. `audience` for Auth0).
    #[serde(default)]
    pub extra: HashMap<String, String>,
    /// Refresh this many seconds BEFORE expiry. Default: 300.
    #[serde(default = "default_refresh_buffer_secs")]
    pub refresh_buffer_secs: u64,
}

fn default_refresh_buffer_secs() -> u64 {
    300
}

/// Active credentials. Stored in SQLite as JSON; the access_token
/// and refresh_token fields are inside the encrypted blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthCredentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub scopes: Vec<String>,
}

impl OAuthCredentials {
    pub fn needs_refresh(&self, buffer_secs: u64) -> bool {
        match self.expires_at {
            Some(exp) => {
                let now = chrono::Utc::now();
                let buffer = chrono::Duration::seconds(buffer_secs as i64);
                exp - buffer <= now
            }
            None => false, // no expiry → assume long-lived
        }
    }
}

#[derive(Clone)]
pub struct OAuthManager {
    db: Db,
    key_store: KeyStore,
    /// In-memory cache of decrypted credentials per provider.
    cache: Arc<RwLock<HashMap<String, OAuthCredentials>>>,
    /// Per-provider OAuth config (refresh URL etc). Persisted in
    /// `oauth_config` table.
    configs: Arc<RwLock<HashMap<String, OAuthConfig>>>,
    /// In-memory state tokens for popup_oauth. Keyed by the state
    /// string the provider redirects back with; value is the
    /// provider_id we issued it for.
    states: Arc<RwLock<HashMap<String, String>>>,
    /// PKCE verifiers keyed by the same state string. Consumed on
    /// `complete_popup_oauth` and removed. Stored separately so the
    /// `states` map stays a `String → String` shape.
    pkce_verifiers: Arc<RwLock<HashMap<String, String>>>,
    /// In-memory device_codes for device_code. Keyed by the
    /// device_code string; value is the provider_id it was
    /// issued for.
    devices: Arc<RwLock<HashMap<String, String>>>,
    http: reqwest::Client,
}

impl OAuthManager {
    pub fn new(db: Db, key_store: KeyStore, http: reqwest::Client) -> Self {
        Self {
            db,
            key_store,
            cache: Arc::new(RwLock::new(HashMap::new())),
            configs: Arc::new(RwLock::new(HashMap::new())),
            states: Arc::new(RwLock::new(HashMap::new())),
            pkce_verifiers: Arc::new(RwLock::new(HashMap::new())),
            devices: Arc::new(RwLock::new(HashMap::new())),
            http,
        }
    }

    /// Store the initial refresh_token for a provider. The first
    /// refresh will populate the access_token + expiry.
    pub async fn setup(&self, cfg: OAuthConfig, initial_refresh_token: &str) -> anyhow::Result<()> {
        let provider_id = cfg.provider_id.clone();
        self.configs
            .write()
            .await
            .insert(provider_id.clone(), cfg.clone());
        // Persist the config
        let pid_for_db = provider_id.clone();
        let cfg_for_db = cfg.clone();
        self.db
            .with(move |conn| {
                conn.execute(
                    r#"INSERT INTO oauth_config
                       (provider_id, token_url, client_id, client_secret, extra_json, refresh_buffer_secs)
                       VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                       ON CONFLICT(provider_id) DO UPDATE SET
                         token_url = excluded.token_url,
                         client_id = excluded.client_id,
                         client_secret = excluded.client_secret,
                         extra_json = excluded.extra_json,
                         refresh_buffer_secs = excluded.refresh_buffer_secs"#,
                    rusqlite::params![
                        pid_for_db,
                        cfg_for_db.token_url,
                        cfg_for_db.client_id,
                        cfg_for_db.client_secret.as_deref(),
                        serde_json::to_string(&cfg_for_db.extra).unwrap_or_else(|_| "{}".to_string()),
                        cfg_for_db.refresh_buffer_secs as i64,
                    ],
                )?;
                Ok(())
            })
            .await?;
        // Store the refresh token encrypted via the keystore.
        self.key_store
            .set(&format!("oauth:{provider_id}"), initial_refresh_token)
            .await?;
        Ok(())
    }

    /// Get a fresh access_token for a provider. Returns the cached
    /// token if it has >refresh_buffer_secs left, otherwise refreshes.
    pub async fn access_token(&self, provider_id: &str) -> anyhow::Result<Option<String>> {
        // Try cache first
        {
            let g = self.cache.read().await;
            if let Some(c) = g.get(provider_id) {
                let cfg = self.configs.read().await.get(provider_id).cloned();
                let buffer = cfg.as_ref().map(|c| c.refresh_buffer_secs).unwrap_or(300);
                if !c.needs_refresh(buffer) {
                    return Ok(Some(c.access_token.clone()));
                }
            }
        }
        // Need to refresh. Load config + refresh token.
        self.refresh(provider_id).await
    }

    /// Store a refresh token for a provider. The provider's OAuth
    /// config (token URL + client ID) is auto-detected from the
    /// manifest for the 3 built-in OAuth providers (github-copilot,
    /// responses, kiro). For custom providers, call `setup()` with
    /// an explicit config.
    pub async fn set_refresh_token(
        &self,
        provider_id: &str,
        token: &str,
    ) -> anyhow::Result<()> {
        self.key_store
            .set(&format!("oauth:{provider_id}"), token)
            .await?;
        // Invalidate the in-memory access token so the next call
        // gets a fresh one.
        self.cache.write().await.remove(provider_id);
        Ok(())
    }

    /// Force a refresh.
    pub async fn refresh(&self, provider_id: &str) -> anyhow::Result<Option<String>> {
        // First, try the manifest's OAuth config (most providers).
        // If absent, fall back to the per-provider oauth_config table
        // (advanced users who want to override the manifest).
        let cfg = self
            .configs
            .read()
            .await
            .get(provider_id)
            .cloned()
            .or_else(|| {
                use crate::providers::manifest;
                let pt = crate::providers::resolve_alias(provider_id);
                pt.and_then(|pt| {
                    manifest::lookup(pt).and_then(|m| {
                        m.oauth.map(|o| crate::oauth::OAuthConfig {
                            provider_id: provider_id.to_string(),
                            token_url: o.token_url.to_string(),
                            client_id: o.client_id.to_string(),
                            client_secret: None,
                            extra: std::collections::HashMap::new(),
                            refresh_buffer_secs: 300,
                        })
                    })
                })
            });
        let refresh_token = self
            .key_store
            .get(&format!("oauth:{provider_id}"))
            .await
            .ok()
            .flatten();
        let cfg = match cfg {
            Some(c) => c,
            None => return Ok(None),
        };
        let refresh_token = match refresh_token {
            Some(t) => t,
            None => return Ok(None),
        };
        let mut body = serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": cfg.client_id,
        });
        if let Some(secret) = &cfg.client_secret {
            body["client_secret"] = json!(secret);
        }
        for (k, v) in &cfg.extra {
            body[k] = json!(v);
        }

        let resp = self
            .http
            .post(&cfg.token_url)
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("OAuth refresh for {provider_id} returned {status}: {text}");
        }
        let v: serde_json::Value = resp.json().await?;
        let new_access = v
            .get("access_token")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow::anyhow!("OAuth response missing access_token"))?
            .to_string();
        let new_refresh = v
            .get("refresh_token")
            .and_then(|x| x.as_str())
            .map(String::from)
            .or(Some(refresh_token)); // some providers don't rotate
        let expires_at = v
            .get("expires_in")
            .and_then(|x| x.as_u64())
            .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs as i64));
        let scopes = v
            .get("scope")
            .and_then(|x| x.as_str())
            .map(|s| s.split_whitespace().map(String::from).collect())
            .unwrap_or_default();

        let creds = OAuthCredentials {
            access_token: new_access.clone(),
            refresh_token: new_refresh.clone(),
            expires_at,
            scopes,
        };
        self.cache
            .write()
            .await
            .insert(provider_id.to_string(), creds.clone());
        if let Some(rt) = new_refresh {
            self.key_store
                .set(&format!("oauth:{provider_id}"), &rt)
                .await?;
        }
        Ok(Some(new_access))
    }
}

/// Background task: refresh every provider's token every 5 minutes
/// if it's about to expire.
pub fn spawn_refresher(manager: OAuthManager) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(300)).await;
            let provider_ids: Vec<String> = manager
                .configs
                .read()
                .await
                .keys()
                .cloned()
                .collect();
            for pid in provider_ids {
                if let Err(e) = manager.refresh(&pid).await {
                    tracing::warn!(provider = %pid, error = %e, "OAuth refresh failed");
                }
            }
        }
    });
}

// ─── popup_oauth (OAuth2 authorization-code redirect) ───────────────────

impl OAuthManager {
/// Generate a state token and build the authorize URL for a popup_oauth
/// provider. The user visits the URL, logs in, gets redirected to
/// the callback endpoint with `?code=...&state=...`.
pub async fn start_popup_oauth(
    &self,
    provider_id: &str,
    redirect_uri: &str,
) -> anyhow::Result<(String, String)> {
    let cfg = lookup_manifest_oauth(provider_id)
        .ok_or_else(|| anyhow::anyhow!("provider {} has no OAuth config or is not popup_oauth", provider_id))?;
    if cfg.authorize_url.is_empty() {
        anyhow::bail!("provider {} is not popup_oauth (no authorize_url)", provider_id);
    }
    if cfg.is_anthropic_paste_code {
        anyhow::bail!(
            "provider {} uses the paste-code flow (POST /admin/oauth/{}/paste) not popup",
            provider_id, provider_id
        );
    }
    let state = format!(
        "{}.{}",
        provider_id,
        uuid::Uuid::new_v4().simple()
    );
    // PKCE: generate a random 64-byte URL-safe verifier, SHA-256 hash
    // it into a challenge. We send only the challenge on the wire at
    // authorize time; the verifier comes back via the redirect.
    let verifier = generate_pkce_verifier();
    let challenge = pkce_challenge_s256(&verifier);

    // Build the authorize URL. We append cfg.scope verbatim
    // (manifest pre-encodes the spaces) plus cfg.extra_authorize_params.
    // If cfg.redirect_uri is set (xAI), we use that instead of the
    // caller's redirect_uri.
    let effective_redirect = if cfg.redirect_uri.is_empty() {
        redirect_uri.to_string()
    } else {
        cfg.redirect_uri.to_string()
    };
    let scope = if cfg.scope.is_empty() {
        "openid profile email offline_access".to_string()
    } else {
        cfg.scope.replace(' ', "+")
    };
    let mut url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&state={}&scope={}",
        cfg.authorize_url,
        urlencoding(&cfg.client_id),
        urlencoding(&effective_redirect),
        urlencoding(&state),
        urlencoding(&scope),
    );
    for (k, v) in cfg.extra_authorize_params {
        url.push_str(&format!("&{}={}", urlencoding(k), urlencoding(v)));
    }
    if cfg.requires_pkce {
        url.push_str(&format!(
            "&code_challenge={}&code_challenge_method=S256",
            urlencoding(&challenge)
        ));
    }

    // Store state + PKCE verifier for verification on callback.
    self.states
        .write()
        .await
        .insert(state.clone(), provider_id.to_string());
    self.pkce_verifiers
        .write()
        .await
        .insert(state.clone(), verifier);
    Ok((url, state))
}

/// Handle the redirect from the OAuth provider. Verifies state,
/// exchanges code for tokens, stores the refresh_token. Idempotent
/// on state (the same state won't be processed twice).
pub async fn complete_popup_oauth(
    &self,
    provider_id: &str,
    code: &str,
    state: &str,
    redirect_uri: &str,
) -> anyhow::Result<()> {
    // Verify state
    let expected_provider = {
        let mut states = self.states.write().await;
        match states.remove(state) {
            Some(p) => p,
            None => anyhow::bail!("invalid or expired state"),
        }
    };
    if expected_provider != provider_id {
        anyhow::bail!("state was issued for a different provider");
    }

    let cfg = lookup_manifest_oauth(provider_id)
        .ok_or_else(|| anyhow::anyhow!("provider {} has no OAuth config", provider_id))?;

    // Look up the PKCE verifier we stored at authorize time and
    // remove it. If missing, this provider either didn't issue
    // PKCE or the state was forged; the OAuth server will reject
    // a missing verifier for PKCE-required flows. We always send
    // it anyway (Google accepts PKCE-only requests, OpenAI
    // requires it).
    let verifier = self.pkce_verifiers.write().await.remove(state);

    let effective_redirect = if cfg.redirect_uri.is_empty() {
        redirect_uri.to_string()
    } else {
        cfg.redirect_uri.to_string()
    };

    let mut body = serde_json::json!({
        "grant_type": "authorization_code",
        "code": code,
        "redirect_uri": effective_redirect,
        "client_id": cfg.client_id,
    });
    if let Some(v) = verifier {
        body["code_verifier"] = serde_json::Value::String(v);
    }
    let client_secret = resolve_client_secret(provider_id, cfg.client_secret);
    if !client_secret.is_empty() {
        body["client_secret"] = serde_json::Value::String(client_secret);
    }

    let resp = self
        .http
        .post(cfg.token_url)
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("token exchange failed: {} {}: {}", status.as_u16(), status.canonical_reason().unwrap_or(""), text);
    }
    let v: serde_json::Value = resp.json().await?;
    let new_refresh = v
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow::anyhow!("token response missing refresh_token"))?
        .to_string();
    self.set_refresh_token(provider_id, &new_refresh).await?;
    Ok(())
}

// ─── device_code (OAuth2 device authorization grant) ────────────────────

/// Start a device-code flow. Returns the user-visible code, the
/// verification URL, the polling interval, and the device_code
/// (used by the client to poll).
pub async fn start_device_flow(
    &self,
    provider_id: &str,
) -> anyhow::Result<DeviceFlowInfo> {
    let cfg = lookup_manifest_oauth(provider_id)
        .ok_or_else(|| anyhow::anyhow!("provider {} has no OAuth config", provider_id))?;
    if cfg.device_code_url.is_empty() {
        anyhow::bail!("provider {} is not device_code", provider_id);
    }
    let body = serde_json::json!({
        "client_id": cfg.client_id,
        "scope": "openid profile email offline_access",
    });
    let resp = self
        .http
        .post(cfg.device_code_url)
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("device_code request failed: {} {}: {}", status.as_u16(), status.canonical_reason().unwrap_or(""), text);
    }
    let v: serde_json::Value = resp.json().await?;
    let device_code = v
        .get("device_code")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow::anyhow!("device_code response missing device_code"))?
        .to_string();
    let user_code = v
        .get("user_code")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow::anyhow!("device_code response missing user_code"))?
        .to_string();
    let verification_uri = v
        .get("verification_uri")
        .or_else(|| v.get("verification_url"))
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow::anyhow!("device_code response missing verification_uri"))?
        .to_string();
    let interval = v
        .get("interval")
        .and_then(|x| x.as_u64())
        .unwrap_or(5);
    let expires_in = v
        .get("expires_in")
        .and_then(|x| x.as_u64())
        .unwrap_or(600);

    // Store device_code for poll
    self.devices
        .write()
        .await
        .insert(device_code.clone(), provider_id.to_string());

    Ok(DeviceFlowInfo {
        device_code,
        user_code,
        verification_uri,
        interval,
        expires_in,
    })
}

/// Poll the device-token endpoint. Returns Ok(true) on success
/// (refresh_token stored), Ok(false) on pending (user hasn't
/// approved yet), Err on rejection or other terminal error.
pub async fn poll_device_flow(
    &self,
    device_code: &str,
) -> anyhow::Result<bool> {
    let provider_id = {
        let devices = self.devices.read().await;
        devices.get(device_code).cloned()
    };
    let provider_id = match provider_id {
        Some(p) => p,
        None => anyhow::bail!("device_code not found (expired or never issued)"),
    };
    let cfg = lookup_manifest_oauth(&provider_id)
        .ok_or_else(|| anyhow::anyhow!("provider {} has no OAuth config", provider_id))?;

    let body = serde_json::json!({
        "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
        "device_code": device_code,
        "client_id": cfg.client_id,
    });
    let resp = self
        .http
        .post(cfg.token_url)
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await?;

    if status.is_success() {
        let new_refresh = body
            .get("refresh_token")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow::anyhow!("token response missing refresh_token"))?
            .to_string();
        self.set_refresh_token(&provider_id, &new_refresh).await?;
        // Clean up the device_code entry
        self.devices.write().await.remove(device_code);
        return Ok(true);
    }

    // Standard device_code error responses:
    // authorization_pending — user hasn't approved yet, keep polling
    // slow_down — interval too short, back off
    // expired_token / access_denied — terminal, remove device_code
    let err = body.get("error").and_then(|x| x.as_str()).unwrap_or("");
    match err {
        "authorization_pending" => Ok(false),
        "slow_down" => Ok(false), // client should slow down
        "expired_token" | "access_denied" => {
            self.devices.write().await.remove(device_code);
            anyhow::bail!("device_code rejected: {}", err);
        }
        _ => anyhow::bail!("device_code poll failed: {} {}: {}", status.as_u16(), err, body),
    }
}
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DeviceFlowInfo {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: u64,
    pub expires_in: u64,
}

/// Look up the manifest's OAuth config for a provider, returning
/// None for providers without OAuth or unknown providers.
fn lookup_manifest_oauth(
    provider_id: &str,
) -> Option<crate::providers::manifest::ManifestOAuth> {
    use crate::providers::manifest;
    let pt = crate::providers::resolve_alias(provider_id)?;
    manifest::lookup(pt).and_then(|m| m.oauth)
}

/// Resolve the client_secret for a provider. Most public OAuth
/// clients (OpenAI Codex, xAI, Anthropic) don't need a secret; the
/// manifest leaves `client_secret` empty and we skip sending it.
/// Google Gemini's gemini-cli client DOES have a public secret — it's
/// shipped in the open-source gemini-cli source, but GitHub's
/// push-protection blocks the literal string. We read it from the
/// `GOOGLE_OAUTH_CLIENT_SECRET` env var when the user wants that
/// flow; otherwise the manifest entry stays empty and Google flow
/// works only for the API-key path.
fn resolve_client_secret(provider_id: &str, manifest_secret: &str) -> String {
    if !manifest_secret.is_empty() {
        return manifest_secret.to_string();
    }
    let env_key = match provider_id {
        "google" => Some("GOOGLE_OAUTH_CLIENT_SECRET"),
        _ => None,
    };
    match env_key {
        Some(k) => std::env::var(k).unwrap_or_default(),
        None => String::new(),
    }
}

fn urlencoding(s: &str) -> String {
    // Minimal percent-encoding for URL query values. Avoids
    // pulling in the `urlencoding` crate.
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push_str(&format!("%{:02X}", b));
            }
        }
    }
    out
}

// ─── PKCE helpers (RFC 7636) ────────────────────────────────────────────

/// Generate a 43-char URL-safe PKCE verifier per RFC 7636 §4.1.
/// We use 32 random bytes → 43 base64url chars (no padding).
fn generate_pkce_verifier() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64_url_encode(&bytes)
}

/// Compute the S256 PKCE challenge: base64url(sha256(verifier))
/// per RFC 7636 §4.2.
fn pkce_challenge_s256(verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(verifier.as_bytes());
    let digest = h.finalize();
    base64_url_encode(digest.as_slice())
}

/// Lower-case base64url encoding without padding (RFC 4648 §5).
fn base64_url_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((bytes.len() * 4 + 2) / 3);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let b0 = bytes[i];
        let b1 = bytes[i + 1];
        let b2 = bytes[i + 2];
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0b11) << 4) | (b1 >> 4)) as usize] as char);
        out.push(ALPHABET[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        out.push(ALPHABET[(b2 & 0x3f) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let b0 = bytes[i];
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[((b0 & 0b11) << 4) as usize] as char);
    } else if rem == 2 {
        let b0 = bytes[i];
        let b1 = bytes[i + 1];
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0b11) << 4) | (b1 >> 4)) as usize] as char);
        out.push(ALPHABET[((b1 & 0x0f) << 2) as usize] as char);
    }
    out
}

// ─── Anthropic paste-code flow ─────────────────────────────────────────

impl OAuthManager {
/// Anthropic's OAuth flow uses a manual paste-code step: the user
/// signs in on the web, the redirect page at console.anthropic.com
/// shows `<authorization_code>#<state>`, and the user copies
/// that string back into the CLI/UI. The "code" half is actually
/// the long-lived refresh token for Claude Code-style routers.
///
/// Accept the pasted string and split it. Store the code portion
/// as the OAuth refresh token; the state portion is just
/// CSRF protection (the user's browser session already validated
/// the flow).
pub async fn paste_anthropic_code(
    &self,
    provider_id: &str,
    pasted: &str,
) -> anyhow::Result<()> {
    let cfg = lookup_manifest_oauth(provider_id)
        .ok_or_else(|| anyhow::anyhow!("provider {} has no OAuth config", provider_id))?;
    if !cfg.is_anthropic_paste_code {
        anyhow::bail!(
            "provider {} does not support the paste-code flow",
            provider_id
        );
    }
    let trimmed = pasted.trim();
    // The format is `<code>#<state>`. Split on the last `#`.
    let (code, _state) = match trimmed.rsplit_once('#') {
        Some((c, s)) => (c.trim(), s.trim()),
        None => {
            // Some users paste just the code. Accept that.
            (trimmed, "")
        }
    };
    if code.is_empty() {
        anyhow::bail!("pasted code is empty");
    }
    // Anthropic uses long-lived codes (~1y) — store as the
    // refresh token directly. The refresh path will detect the
    // format and try to refresh; if the server rejects, the
    // user re-runs the paste flow.
    self.set_refresh_token(provider_id, code).await?;
    Ok(())
}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_verifier_is_url_safe_43_chars() {
        let v = generate_pkce_verifier();
        assert_eq!(v.len(), 43, "PKCE verifier should be 43 chars, got {}", v.len());
        for c in v.chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '-' || c == '_',
                "PKCE verifier contains unexpected char: {c}"
            );
        }
    }

    #[test]
    fn pkce_challenge_matches_rfc7636_test_vector() {
        // From RFC 7636 Appendix B:
        //   verifier  = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
        //   challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(pkce_challenge_s256(verifier), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn base64_url_encode_matches_known_vector() {
        // Empty
        assert_eq!(base64_url_encode(&[]), "");
        // "f" → "Zm8" (RFC 4648 §10)
        assert_eq!(base64_url_encode(b"fo"), "Zm8");
        // "foo" → "Zm9v"
        assert_eq!(base64_url_encode(b"foo"), "Zm9v");
        // "foob" → "Zm9vYg"
        assert_eq!(base64_url_encode(b"foob"), "Zm9vYg");
        // "fooba" → "Zm9vYmE"
        assert_eq!(base64_url_encode(b"fooba"), "Zm9vYmE");
        // "foobar" → "Zm9vYmFy"
        assert_eq!(base64_url_encode(b"foobar"), "Zm9vYmFy");
    }
}

#[cfg(test)]
mod popup_url_tests {
    use super::*;

    fn build_authorize_url(
        cfg: &crate::providers::manifest::ManifestOAuth,
        redirect_uri: &str,
        state: &str,
        verifier: &str,
    ) -> String {
        let challenge = pkce_challenge_s256(verifier);
        // xAI's OAuth client is registered with a 127.0.0.1 callback
        // (not the server's URL). Other providers with `redirect_uri`
        // set in the manifest override the caller-supplied redirect too.
        let effective_redirect = if cfg.redirect_uri.is_empty() {
            redirect_uri.to_string()
        } else {
            cfg.redirect_uri.to_string()
        };
        let scope = if cfg.scope.is_empty() {
            "openid profile email offline_access".to_string()
        } else {
            cfg.scope.replace(' ', "+")
        };
        let mut url = format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&state={}&scope={}",
            cfg.authorize_url,
            urlencoding(&cfg.client_id),
            urlencoding(&effective_redirect),
            urlencoding(state),
            urlencoding(&scope),
        );
        for (k, v) in cfg.extra_authorize_params {
            url.push_str(&format!("&{}={}", urlencoding(k), urlencoding(v)));
        }
        if cfg.requires_pkce {
            url.push_str(&format!(
                "&code_challenge={}&code_challenge_method=S256",
                urlencoding(&challenge)
            ));
        }
        url
    }

    #[test]
    fn openai_popup_url_uses_real_client_id_and_scope() {
        let cfg = lookup_manifest_oauth("openai").expect("openai manifest");
        let url = build_authorize_url(&cfg, "http://localhost:8080/admin/oauth/openai/callback", "openai.abc123", "verifier_xyz");
        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"), "got {url}");
        assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"), "missing real client_id");
        // Scope is space-joined then url-encoded → `+` becomes `%2B`.
        for s in &["openid", "profile", "email", "offline_access"] {
            assert!(
                url.contains(s),
                "scope missing {s} in URL: {url}"
            );
        }
        assert!(url.contains("code_challenge="), "missing PKCE challenge");
        assert!(url.contains("code_challenge_method=S256"), "missing PKCE method");
    }

    #[test]
    fn google_popup_url_uses_real_client_id_and_extra_params() {
        let cfg = lookup_manifest_oauth("google").expect("google manifest");
        let url = build_authorize_url(&cfg, "http://localhost:8080/admin/oauth/google/callback", "google.abc", "verifier");
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(url.contains("client_id=681255809395-oo8ft2oprdrnp9e3aqf6av3hmi99ikee6.apps.googleusercontent.com"));
        assert!(url.contains("access_type=offline"), "Google requires access_type=offline for refresh tokens");
        assert!(url.contains("prompt=consent"));
        assert!(url.contains("code_challenge="));
    }

    #[test]
    fn xai_popup_url_uses_overridden_redirect_uri() {
        let cfg = lookup_manifest_oauth("xai").expect("xai manifest");
        let url = build_authorize_url(&cfg, "http://127.0.0.1:8080/admin/oauth/xai/callback", "xai.abc", "verifier");
        // xAI's OAuth client is registered with a 127.0.0.1 redirect on
        // a bare /callback path — NOT the server's callback URL.
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fcallback"),
                "xAI redirect must override, got {url}");
        assert!(url.contains("client_id=b1a00492-073a-47ea-816f-4c329264a828"));
        assert!(url.contains("grok-cli%3Aaccess"), "xAI-specific scope");
    }

    #[test]
    fn anthropic_paste_code_marker_set() {
        let cfg = lookup_manifest_oauth("anthropic").expect("anthropic manifest");
        assert!(cfg.is_anthropic_paste_code, "anthropic should be paste-code flow");
        assert_eq!(cfg.client_id, "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
        assert_eq!(
            cfg.paste_code_redirect_url,
            "https://console.anthropic.com/oauth/code/callback"
        );
    }

    #[test]
    fn copilot_device_code_marker() {
        let cfg = lookup_manifest_oauth("github-copilot").expect("copilot manifest");
        assert!(cfg.device_code_url.contains("github.com/login/device/code"));
        assert_eq!(cfg.client_id, "Iv1.b507a08c87ecfe98");
        assert!(!cfg.requires_pkce, "device code flow doesn't need PKCE");
        assert!(!cfg.is_anthropic_paste_code);
    }

    #[test]
    fn kiro_device_code_marker() {
        let cfg = lookup_manifest_oauth("kiro").expect("kiro manifest");
        assert!(cfg.device_code_url.contains("kiro.dev/deviceAuthorization"));
        assert!(!cfg.requires_pkce);
    }

    #[test]
    fn no_provider_has_empty_client_id() {
        // Catch the previous bug where client_ids were placeholders
        // like "openai-cli-public" or "xai-cli-public". Kiro is the
        // one exception — it dynamically registers a client_id+secret
        // via `registerClient` before the device flow starts, so the
        // manifest stores the *type* ("kiro-cli") rather than the
        // real id (which doesn't exist until runtime).
        for provider in &[
            "anthropic", "openai", "responses", "google", "xai",
            "github-copilot", "minimax",
        ] {
            let cfg = lookup_manifest_oauth(provider)
                .unwrap_or_else(|| panic!("{provider} has no manifest oauth"));
            assert!(
                !cfg.client_id.is_empty(),
                "{provider}: client_id is empty"
            );
            assert!(
                cfg.client_id.len() >= 8,
                "{}: client_id '{}' looks like a placeholder",
                provider, cfg.client_id
            );
            // Reject obvious placeholder patterns.
            assert!(
                !cfg.client_id.ends_with("-public"),
                "{}: client_id '{}' is the old placeholder (ends in -public)",
                provider, cfg.client_id
            );
        }
        // Kiro uses dynamic registration — the literal "kiro-cli" is
        // intentional, the real id comes from the OIDC register step.
        let kiro = lookup_manifest_oauth("kiro").expect("kiro manifest");
        assert_eq!(kiro.client_id, "kiro-cli");
        assert_eq!(
            kiro.device_code_url,
            "https://prod.us-east-1.auth.desktop.kiro.dev/deviceAuthorization"
        );
    }
}
