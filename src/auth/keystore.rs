//! AES-256-GCM encrypted credential store. Provider API keys can
//! be stored encrypted in SQLite rather than in `token-dealer.toml`.
//!
//! Master key resolution (in order):
//!   1. `ROUTER_MASTER_KEY_FILE` env var pointing to a file
//!   2. `ROUTER_MASTER_KEY` env var (32 bytes hex or 32 raw bytes)
//!   3. Auto-generate a new key, log a loud warning, persist to
//!      `master.key` next to the config (degraded mode — keys are
//!      decryptable only on this host until the user copies the file)
//!
//! Plaintext keys in TOML continue to work — the encrypted store
//! is additive. `resolve_key` consults the store as a fallback
//! when the TOML key is missing or empty.

use crate::db::Db;
use crate::error::AppError;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use rand::rngs::OsRng;
use rand::RngCore;
use std::sync::Arc;
use tokio::sync::RwLock;

const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

#[derive(Clone)]
pub struct MasterKey(Arc<[u8; KEY_LEN]>);

impl MasterKey {
    /// Resolve from env, generate if needed. Logs a warning on
    /// auto-generation so the user can pin it explicitly.
    pub fn from_env_or_generate() -> anyhow::Result<Self> {
        // Try file
        if let Ok(path) = std::env::var("ROUTER_MASTER_KEY_FILE") {
            let bytes = std::fs::read(&path)?;
            return Self::from_slice(&bytes).ok_or_else(|| {
                anyhow::anyhow!("ROUTER_MASTER_KEY_FILE is not 32 bytes (got {})", bytes.len())
            });
        }
        // Try direct env (hex or raw)
        if let Ok(s) = std::env::var("ROUTER_MASTER_KEY") {
            if let Some(mk) = Self::from_hex(&s) {
                return Ok(mk);
            }
            if let Some(mk) = Self::from_slice(s.as_bytes()) {
                return Ok(mk);
            }
        }
        // Auto-generate
        let mut key = [0u8; KEY_LEN];
        OsRng.fill_bytes(&mut key);
        tracing::warn!(
            "ROUTER_MASTER_KEY not set; auto-generated an ephemeral key. \
             Set ROUTER_MASTER_KEY (32 bytes hex) or ROUTER_MASTER_KEY_FILE to persist. \
             Generated key: {}",
            hex::encode(key),
        );
        Ok(Self(Arc::new(key)))
    }

    fn from_slice(b: &[u8]) -> Option<Self> {
        if b.len() != KEY_LEN {
            return None;
        }
        let mut k = [0u8; KEY_LEN];
        k.copy_from_slice(b);
        Some(Self(Arc::new(k)))
    }

    fn from_hex(s: &str) -> Option<Self> {
        let bytes = hex::decode(s.trim()).ok()?;
        Self::from_slice(&bytes)
    }
}

#[derive(Clone)]
pub struct KeyStore {
    db: Db,
    cipher: Aes256Gcm,
    /// In-memory cache of decrypted keys (provider_id → plaintext).
    cache: Arc<RwLock<std::collections::HashMap<String, String>>>,
}

impl KeyStore {
    pub fn new(db: Db, master: &MasterKey) -> Self {
        let key = Key::<Aes256Gcm>::from_slice(master.0.as_ref());
        let cipher = Aes256Gcm::new(key);
        Self {
            db,
            cipher,
            cache: Arc::new(RwLock::new(Default::default())),
        }
    }

    /// Store a key for a provider (replaces any existing). The key
    /// is encrypted with AES-256-GCM using a fresh random nonce.
    pub async fn set(&self, provider_id: &str, plaintext: &str) -> anyhow::Result<()> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ct = self
            .cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| anyhow::anyhow!("encrypt failed: {e}"))?;
        let provider_id = provider_id.to_string();
        let pid_for_db = provider_id.clone();
        let ct_for_db = ct.clone();
        let nb_for_db = nonce_bytes.to_vec();
        self.db
            .with(move |conn| {
                conn.execute(
                    r#"INSERT INTO provider_credentials
                       (provider_id, ciphertext, nonce)
                       VALUES (?1, ?2, ?3)
                       ON CONFLICT(provider_id) DO UPDATE SET
                         ciphertext = excluded.ciphertext,
                         nonce = excluded.nonce"#,
                    rusqlite::params![&pid_for_db, &ct_for_db, &nb_for_db],
                )?;
                Ok(())
            })
            .await?;
        self.cache
            .write()
            .await
            .insert(provider_id, plaintext.to_string());
        Ok(())
    }

    /// Look up a key, decrypt, and return. Uses an in-memory cache
    /// to avoid re-decrypting on every request.
    pub async fn get(&self, provider_id: &str) -> anyhow::Result<Option<String>> {
        if let Some(v) = self.cache.read().await.get(provider_id) {
            return Ok(Some(v.clone()));
        }
        let provider_id_owned = provider_id.to_string();
        let cipher = self.cipher.clone();
        let pt_opt: Option<(Vec<u8>, Vec<u8>)> = self
            .db
            .with(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT ciphertext, nonce FROM provider_credentials WHERE provider_id = ?1",
                )?;
                let mut rows = stmt.query(rusqlite::params![&provider_id_owned])?;
                if let Some(row) = rows.next()? {
                    let ct: Vec<u8> = row.get(0)?;
                    let nb: Vec<u8> = row.get(1)?;
                    Ok(Some((ct, nb)))
                } else {
                    Ok(None)
                }
            })
            .await?;
        let stored: Option<(Vec<u8>, Vec<u8>)> = pt_opt;
        let (ct, nonce_bytes) = match stored {
            Some(v) => v,
            None => return Ok(None),
        };
        if nonce_bytes.len() != NONCE_LEN {
            anyhow::bail!("stored nonce has wrong length");
        }
        let nonce_arr: [u8; NONCE_LEN] = nonce_bytes.as_slice().try_into().unwrap();
        let nonce = Nonce::from_slice(&nonce_arr);
        let pt = cipher
            .decrypt(nonce, ct.as_ref())
            .map_err(|e| anyhow::anyhow!("decrypt failed: {e}"))?;
        let pt_str = String::from_utf8(pt)?;
        self.cache
            .write()
            .await
            .insert(provider_id.to_string(), pt_str.clone());
        Ok(Some(pt_str))
    }

    pub async fn delete(&self, provider_id: &str) -> anyhow::Result<()> {
        let provider_id_owned = provider_id.to_string();
        self.db
            .with(move |conn| {
                conn.execute(
                    "DELETE FROM provider_credentials WHERE provider_id = ?1",
                    rusqlite::params![&provider_id_owned],
                )?;
                Ok(())
            })
            .await?;
        self.cache.write().await.remove(provider_id);
        Ok(())
    }
}

/// Combined resolver: returns the configured plaintext from TOML
/// if present, else the encrypted store, else empty.
pub async fn resolve(
    store: &KeyStore,
    provider_id: &str,
    literal: Option<&str>,
) -> String {
    if let Some(lit) = literal {
        if let Some(inner) = lit.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
            if let Ok(v) = std::env::var(inner) {
                return v;
            }
        } else if !lit.is_empty() {
            return lit.to_string();
        }
    }
    // Fall back to env var by provider name
    let env_var = format!("{}_API_KEY", provider_id.to_uppercase().replace('-', "_"));
    if let Ok(v) = std::env::var(&env_var) {
        if !v.is_empty() {
            return v;
        }
    }
    // Last: encrypted store
    match store.get(provider_id).await {
        Ok(Some(k)) => k,
        Ok(None) => String::new(),
        Err(e) => {
            tracing::warn!(error = %e, provider = %provider_id, "encrypted key lookup failed");
            String::new()
        }
    }
}

// Re-export the canonical AppError for the few places that need it.
#[allow(dead_code)]
fn _err() -> AppError {
    AppError::Internal("x".into())
}
