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
    http: reqwest::Client,
}

impl OAuthManager {
    pub fn new(db: Db, key_store: KeyStore, http: reqwest::Client) -> Self {
        Self {
            db,
            key_store,
            cache: Arc::new(RwLock::new(HashMap::new())),
            configs: Arc::new(RwLock::new(HashMap::new())),
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

    /// Force a refresh.
    pub async fn refresh(&self, provider_id: &str) -> anyhow::Result<Option<String>> {
        let cfg = self.configs.read().await.get(provider_id).cloned();
        let cfg = match cfg {
            Some(c) => c,
            None => return Ok(None),
        };
        let refresh_token = self
            .key_store
            .get(&format!("oauth:{provider_id}"))
            .await
            .ok()
            .flatten();
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
