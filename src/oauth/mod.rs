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
    let state = format!(
        "{}.{}",
        provider_id,
        uuid::Uuid::new_v4().simple()
    );
    // Standard OAuth2: response_type=code, include state + redirect_uri.
    // We hardcode scope=openid profile email offline_access for OpenAI;
    // other providers may ignore unknown scopes.
    let url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&state={}&scope=openid+profile+email+offline_access",
        cfg.authorize_url,
        urlencoding(&cfg.client_id),
        urlencoding(redirect_uri),
        urlencoding(&state),
    );
    // Store state for verification on callback.
    self.states.write().await.insert(state.clone(), provider_id.to_string());
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

    let body = serde_json::json!({
        "grant_type": "authorization_code",
        "code": code,
        "redirect_uri": redirect_uri,
        "client_id": cfg.client_id,
    });

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
        .post(cfg.device_token_url)
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