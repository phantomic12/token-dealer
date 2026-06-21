//! AES-256-GCM encrypted credential store. Provider API keys can
//! be stored encrypted in SQLite rather than in `token-dealer.toml`.
//!
//! Master key resolution (in order, v0.2.0):
//!   1. `ROUTER_MASTER_KEY_FILE` env var pointing to a file
//!   2. `ROUTER_MASTER_KEY` env var (32 bytes hex or base64)
//!   3. `from_env_or_generate()` falls back to ephemeral key with
//!      a loud warning; `from_env_strict()` refuses.
//!
//! Plaintext keys in TOML continue to work — the encrypted store
//! is additive. `resolve_key` consults the store as a fallback
//! when the TOML key is missing or empty, and decrypts
//! `enc:<...>` prefixed values when a master key is present.
//!
//! On-disk format for encrypted values: `enc:<base64(nonce ||
//! ciphertext || tag)>` where nonce is 12 bytes and tag is the
//! 16-byte GCM tag. Encryption is AES-256-GCM with an HKDF-SHA256
//! subkey derived from the master key. No passphrase, no Argon2 —
//! the master key itself is the secret; protect it.

use crate::db::Db;
use crate::error::AppError;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine;
use hkdf::Hkdf;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::Sha256;
use std::sync::Arc;
use tokio::sync::RwLock;

const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
const ENC_PREFIX: &str = "enc:";

/// Domain error for the master key. The CLI surfaces the
/// `Display` message verbatim at startup, so keep it
/// actionable.
#[derive(Debug, thiserror::Error)]
pub enum MasterKeyError {
    #[error(
        "ROUTER_MASTER_KEY (or ROUTER_MASTER_KEY_FILE) is required when [auth] is enabled. \
         Generate one with: head -c 32 /dev/urandom | base64"
    )]
    Missing,
    #[error("ROUTER_MASTER_KEY_FILE is not 32 bytes (got {0})")]
    BadFileLength(usize),
    #[error("ROUTER_MASTER_KEY could not be decoded as hex (64 chars) or base64 (44 chars) or raw 32 bytes")]
    BadFormat,
    #[error("decrypt failed: ciphertext is malformed or wrong master key")]
    Decrypt,
    #[error("encrypted blob is not valid base64")]
    BadBase64,
}

#[derive(Clone)]
pub struct MasterKey(Arc<[u8; KEY_LEN]>);

impl MasterKey {
    /// Strict load. Refuses if the env var is missing — used
    /// when the user has opted in to v0.2.0 hardening (i.e.
    /// `[auth].enabled = true`). Callers should pair this with
    /// a check against the loaded config.
    pub fn from_env_strict() -> Result<Self, MasterKeyError> {
        if let Ok(path) = std::env::var("ROUTER_MASTER_KEY_FILE") {
            let bytes = std::fs::read(&path).map_err(|_| MasterKeyError::BadFormat)?;
            return Self::from_slice(&bytes).ok_or(MasterKeyError::BadFileLength(bytes.len()));
        }
        if let Ok(s) = std::env::var("ROUTER_MASTER_KEY") {
            if let Some(mk) = Self::from_hex(&s) {
                return Ok(mk);
            }
            if let Some(mk) = Self::from_base64(&s) {
                return Ok(mk);
            }
            if let Some(mk) = Self::from_slice(s.as_bytes()) {
                return Ok(mk);
            }
            return Err(MasterKeyError::BadFormat);
        }
        Err(MasterKeyError::Missing)
    }

    /// Legacy load: env var, file, or auto-generate. Logs a
    /// warning on auto-generation so the user can pin it
    /// explicitly. Use only when `[auth]` is disabled.
    pub fn from_env_or_generate() -> anyhow::Result<Self> {
        if let Ok(path) = std::env::var("ROUTER_MASTER_KEY_FILE") {
            let bytes = std::fs::read(&path)?;
            return Self::from_slice(&bytes).ok_or_else(|| {
                anyhow::anyhow!(
                    "ROUTER_MASTER_KEY_FILE is not 32 bytes (got {})",
                    bytes.len()
                )
            });
        }
        if let Ok(s) = std::env::var("ROUTER_MASTER_KEY") {
            if let Some(mk) = Self::from_hex(&s) {
                return Ok(mk);
            }
            if let Some(mk) = Self::from_base64(&s) {
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
             Set ROUTER_MASTER_KEY (32 bytes hex or base64) or \
             ROUTER_MASTER_KEY_FILE to persist. Generated key: {}",
            hex::encode(key),
        );
        Ok(Self(Arc::new(key)))
    }

    /// Derive a 32-byte subkey via HKDF-SHA256. Used to scope a
    /// single master key across multiple domains (TOML keys,
    /// SQLite provider_credentials, future admin password hash)
    /// so rotating one domain doesn't require re-encrypting
    /// another.
    pub fn derive_subkey(&self, info: &[u8]) -> [u8; KEY_LEN] {
        let hk = Hkdf::<Sha256>::new(None, self.0.as_ref());
        let mut out = [0u8; KEY_LEN];
        // `expand` only fails on bogus length; 32 bytes is well
        // inside HKDF-SHA256's 255 * 32 byte limit.
        hk.expand(info, &mut out).expect("hkdf expand");
        out
    }

    /// Encrypt a plaintext string with a subkey derived for
    /// `purpose`. Output: `enc:<base64(nonce || ct || tag)>`.
    pub fn encrypt(&self, plaintext: &str, purpose: &[u8]) -> String {
        let subkey = self.derive_subkey(purpose);
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&subkey));
        let mut nonce_bytes = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ct = cipher
            .encrypt(nonce, plaintext.as_bytes())
            .expect("encryption should not fail with fresh nonce");
        // Concatenate: nonce || ct (tag is appended to ct by aes-gcm)
        let mut blob = Vec::with_capacity(NONCE_LEN + ct.len());
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ct);
        format!(
            "{}{}",
            ENC_PREFIX,
            base64::engine::general_purpose::STANDARD.encode(blob)
        )
    }

    /// Decrypt an `enc:`-prefixed blob with the subkey for
    /// `purpose`. Returns the plaintext. The same purpose must
    /// be used at encrypt time; mismatched purposes yield a
    /// decrypt failure (GCM tag check).
    pub fn decrypt(&self, blob: &str, purpose: &[u8]) -> Result<String, MasterKeyError> {
        let rest = blob
            .strip_prefix(ENC_PREFIX)
            .ok_or(MasterKeyError::Decrypt)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(rest.trim())
            .map_err(|_| MasterKeyError::BadBase64)?;
        if bytes.len() <= NONCE_LEN {
            return Err(MasterKeyError::Decrypt);
        }
        let (nonce_bytes, ct) = bytes.split_at(NONCE_LEN);
        let subkey = self.derive_subkey(purpose);
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&subkey));
        let nonce_arr: [u8; NONCE_LEN] = nonce_bytes
            .try_into()
            .map_err(|_| MasterKeyError::Decrypt)?;
        let nonce = Nonce::from_slice(&nonce_arr);
        let pt = cipher
            .decrypt(nonce, ct)
            .map_err(|_| MasterKeyError::Decrypt)?;
        String::from_utf8(pt).map_err(|_| MasterKeyError::Decrypt)
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

    fn from_base64(s: &str) -> Option<Self> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(s.trim())
            .ok()?;
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

/// Domain separation labels for HKDF. Per the v0.2.0 plan
/// (item 2), each encryption domain gets its own subkey so
/// rotating one doesn't require re-encrypting the others.
pub mod purpose {
    /// Subkey for `[[auth.keys]].key` values in token-dealer.toml.
    pub const TOML_AUTH_KEY: &[u8] = b"token-dealer/v0.2/auth-keys/v1";
    /// Subkey for the SQLite `provider_credentials` table.
    pub const SQLITE_PROVIDER_CRED: &[u8] = b"token-dealer/v0.2/provider-credentials/v1";
}

/// Combined resolver: returns the configured plaintext from TOML
/// if present (decrypting `enc:`-prefixed values), else the
/// encrypted store, else empty.
pub async fn resolve(
    store: &KeyStore,
    master: &MasterKey,
    provider_id: &str,
    literal: Option<&str>,
) -> String {
    if let Some(lit) = literal {
        // Environment variable indirection: literal="${VAR}" → std::env::var(VAR).
        if let Some(inner) = lit.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
            if let Ok(v) = std::env::var(inner) {
                return v;
            }
            // Fall through if the env var is unset — the literal
            // might be a literal (not an indirection) that just
            // happens to start with `$`.
        }
        // `enc:` blob → decrypt with the per-purpose subkey.
        if let Some(blob) = lit.strip_prefix("enc:") {
            match master.decrypt(&format!("enc:{blob}"), purpose::TOML_AUTH_KEY) {
                Ok(pt) => return pt,
                Err(e) => tracing::warn!(
                    error = %e, provider = %provider_id,
                    "failed to decrypt enc:-prefixed key; check ROUTER_MASTER_KEY"
                ),
            }
        }
        if !lit.is_empty() {
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
    // Last: encrypted store (provider credentials table, decrypted
    // by KeyStore using its own subkey).
    match store.get(provider_id).await {
        Ok(Some(k)) => k,
        Ok(None) => String::new(),
        Err(e) => {
            tracing::warn!(error = %e, provider = %provider_id, "encrypted key lookup failed");
            String::new()
        }
    }
}

#[allow(dead_code)]
fn _err() -> AppError {
    AppError::Internal("x".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize the env-mutating tests so they don't race on
    /// the process-global `ROUTER_MASTER_KEY` (and friends).
    /// The mutating tests need to read + write + read the same
    /// env var, and the rust test harness runs them in parallel
    /// by default. A static mutex is enough; the critical
    /// sections are short.
    fn env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    fn make_master() -> MasterKey {
        // 32 raw bytes — never use a hardcoded key in production.
        let bytes: [u8; KEY_LEN] = std::array::from_fn(|i| i as u8);
        MasterKey(Arc::new(bytes))
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let mk = make_master();
        let pt = "sk-very-secret-1234567890";
        let blob = mk.encrypt(pt, purpose::TOML_AUTH_KEY);
        assert!(blob.starts_with("enc:"));
        let recovered = mk.decrypt(&blob, purpose::TOML_AUTH_KEY).unwrap();
        assert_eq!(recovered, pt);
    }

    #[test]
    fn encrypt_produces_unique_nonces() {
        let mk = make_master();
        let blob1 = mk.encrypt("a", purpose::TOML_AUTH_KEY);
        let blob2 = mk.encrypt("a", purpose::TOML_AUTH_KEY);
        assert_ne!(
            blob1, blob2,
            "AES-GCM must use a fresh nonce per encryption"
        );
    }

    #[test]
    fn wrong_purpose_fails_to_decrypt() {
        let mk = make_master();
        let blob = mk.encrypt("secret", purpose::TOML_AUTH_KEY);
        // Different purpose → different subkey → GCM tag fails.
        let err = mk
            .decrypt(&blob, purpose::SQLITE_PROVIDER_CRED)
            .unwrap_err();
        assert!(matches!(err, MasterKeyError::Decrypt));
    }

    #[test]
    fn wrong_master_key_fails_to_decrypt() {
        let mk1 = make_master();
        let mk2 = MasterKey(Arc::new([0xFFu8; KEY_LEN]));
        let blob = mk1.encrypt("secret", purpose::TOML_AUTH_KEY);
        let err = mk2.decrypt(&blob, purpose::TOML_AUTH_KEY).unwrap_err();
        assert!(matches!(err, MasterKeyError::Decrypt));
    }

    #[test]
    fn decrypt_rejects_non_enc_prefix() {
        let mk = make_master();
        let err = mk.decrypt("plaintext", purpose::TOML_AUTH_KEY).unwrap_err();
        assert!(matches!(err, MasterKeyError::Decrypt));
    }

    #[test]
    fn decrypt_rejects_bad_base64() {
        let mk = make_master();
        let err = mk
            .decrypt("enc:!!!not-base64!!!", purpose::TOML_AUTH_KEY)
            .unwrap_err();
        assert!(matches!(err, MasterKeyError::BadBase64));
    }

    #[test]
    fn from_env_strict_refuses_when_missing() {
        let _guard = env_lock().lock().unwrap();
        // Make sure neither env var is set during the test.
        // SAFETY: tests in this module are #[test] and run in
        // parallel within the same process; the env var we set
        // is process-global, so any other test that depends on
        // it would be racy. Tests in this module don't, so it's
        // safe.
        let saved_file = std::env::var("ROUTER_MASTER_KEY_FILE").ok();
        let saved_key = std::env::var("ROUTER_MASTER_KEY").ok();
        // SAFETY: only this test touches these vars; restoring
        // at the end.
        unsafe {
            std::env::remove_var("ROUTER_MASTER_KEY_FILE");
            std::env::remove_var("ROUTER_MASTER_KEY");
        }
        let err = MasterKey::from_env_strict();
        assert!(matches!(err, Err(MasterKeyError::Missing)));
        // Restore (best-effort).
        if let Some(v) = saved_file {
            // SAFETY: see above.
            unsafe {
                std::env::set_var("ROUTER_MASTER_KEY_FILE", v);
            }
        }
        if let Some(v) = saved_key {
            // SAFETY: see above.
            unsafe {
                std::env::set_var("ROUTER_MASTER_KEY", v);
            }
        }
    }

    #[test]
    fn from_env_strict_accepts_hex() {
        let _guard = env_lock().lock().unwrap();
        let bytes = [0x42u8; KEY_LEN];
        let hex = hex::encode(bytes);
        let saved = std::env::var("ROUTER_MASTER_KEY").ok();
        // SAFETY: test-isolated env mutation.
        unsafe {
            std::env::set_var("ROUTER_MASTER_KEY", &hex);
        }
        let mk = MasterKey::from_env_strict().expect("hex should decode");
        assert_eq!(mk.0.as_ref(), &bytes);
        match saved {
            Some(v) => unsafe {
                std::env::set_var("ROUTER_MASTER_KEY", v);
            },
            None => unsafe {
                std::env::remove_var("ROUTER_MASTER_KEY");
            },
        }
    }

    #[test]
    fn from_env_strict_accepts_base64() {
        let _guard = env_lock().lock().unwrap();
        let bytes = [0xA5u8; KEY_LEN];
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        let saved = std::env::var("ROUTER_MASTER_KEY").ok();
        // SAFETY: test-isolated env mutation.
        unsafe {
            std::env::set_var("ROUTER_MASTER_KEY", &b64);
        }
        let mk = MasterKey::from_env_strict().expect("base64 should decode");
        assert_eq!(mk.0.as_ref(), &bytes);
        match saved {
            Some(v) => unsafe {
                std::env::set_var("ROUTER_MASTER_KEY", v);
            },
            None => unsafe {
                std::env::remove_var("ROUTER_MASTER_KEY");
            },
        }
    }

    #[test]
    fn hkdf_subkeys_are_distinct() {
        let mk = make_master();
        let a = mk.derive_subkey(purpose::TOML_AUTH_KEY);
        let b = mk.derive_subkey(purpose::SQLITE_PROVIDER_CRED);
        assert_ne!(a, b, "different purposes must produce different subkeys");
    }
}
