//! ConfigService — load, hold, and serve the active config under a
//! RwLock so handlers can read it lock-free on the hot path.
//! Hot-reload from disk is wired in; UI-driven reload is phase 2.

use super::types::RouterConfig;
use super::validate::{validate as validate_toml, ValidationOutcome};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct ConfigService {
    inner: Arc<RwLock<RouterConfig>>,
    path: Arc<std::path::PathBuf>,
}

impl ConfigService {
    /// Load from a TOML file. Missing file → defaults. Missing fields
    /// inside an existing file → use the field's `Default`.
    ///
    /// Per the v0.2.0 plan (item 5), every load runs the
    /// validator. Hard errors (wrong type, out-of-range, missing
    /// required) refuse the load; warnings (deprecated fields,
    /// unknown fields) are logged once and the load proceeds.
    pub async fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            // v0.2.0 plan item 5 first-run UX: write a minimal
            // config to disk so subsequent runs find something
            // to validate. The shape covers [server], [auth]
            // (with enabled = true and an empty keys list), and
            // an empty providers / tiers list. The admin
            // password is generated separately by
            // `bootstrap_admin_if_needed` and lives in the
            // user_store, not the config file — keeps the
            // password out of any backup / git-tracked artifact.
            Self::write_minimal_config(&path).await?;
            tracing::info!(path = %path.display(), "wrote minimal config (first run)");
            let config = RouterConfig::default();
            return Ok(Self {
                inner: Arc::new(RwLock::new(config)),
                path: Arc::new(path),
            });
        }
        // v0.2.0 plan item 6a: auto-migrate a v0.1.x config in
        // place. Idempotent — a v0.2.0-shaped config (one that
        // already has `[ratelimit]`) is left alone. Migration
        // is non-interactive so `docker compose up -d` works in
        // a non-TTY environment.
        if let Err(e) = Self::migrate_v0_1_if_needed(&path).await {
            tracing::warn!(error = %e, path = %path.display(),
                "auto-migration failed; continuing with the on-disk config as-is");
        }
        let text = tokio::fs::read_to_string(&path).await?;
        let outcome = validate_toml(&text);
        report_outcome(&outcome);
        if outcome.has_errors() {
            anyhow::bail!(
                "config at {} failed validation: {} error(s); run `token-dealer check` for details",
                path.display(),
                outcome.errors.len()
            );
        }
        let config: RouterConfig = toml::from_str(&text)?;
        Ok(Self {
            inner: Arc::new(RwLock::new(config)),
            path: Arc::new(path),
        })
    }

    /// Write a minimal `token-dealer.toml` to `path` for the
    /// first-run case. Idempotent: caller is expected to only
    /// invoke this when the file is missing. The shape is the
    /// minimum needed for the server to start: a [server]
    /// block with the default bind / log level, an [auth]
    /// block with `enabled = true` (so the strict-master-key
    /// gate is in effect), and an empty `[[auth.keys]]` array.
    /// Providers and tiers are absent — the WebUI walks the
    /// user through adding the first one.
    async fn write_minimal_config(path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await.ok();
            }
        }
        let body = "\
# token-dealer first-run config.
# Add providers via /ui/providers or by editing this file.
# See README.md for the full schema and an upgraded example.
[server]
bind = \"0.0.0.0:8080\"
log_level = \"info\"

[auth]
enabled = true

[[auth.keys]]
# name = \"example\"
# key = \"sk-...\"
";
        tokio::fs::write(path, body).await?;
        Ok(())
    }

    /// Detect a v0.1.x config and rewrite it in place to
    /// v0.2.0 shape. Idempotent: a config that already has
    /// `[ratelimit]` is left alone.
    ///
    /// Heuristic (per the plan): pre-v0.2.0 iff the file has
    /// no `[ratelimit]` section. v0.1.x had no such section.
    /// We additionally treat the file as pre-v0.2.0 if any
    /// `[[auth.keys]].key` is plaintext (no `enc:` prefix) —
    /// the encryption feature is v0.2.0-only.
    ///
    /// Actions on a detected v0.1.x:
    ///   1. Copy the current file to `<path>.v0.1.bak`.
    ///   2. Add empty `[ratelimit]` with the plan's defaults.
    ///   3. If `ROUTER_MASTER_KEY` is set, encrypt
    ///      `[[auth.keys]].key` values in place (via
    ///      `MasterKey::encrypt`). Otherwise leave plaintext
    ///      and log a loud warning.
    ///   4. Write the modified text back to disk.
    ///   5. Log a one-line summary.
    async fn migrate_v0_1_if_needed(path: &Path) -> anyhow::Result<()> {
        let text = tokio::fs::read_to_string(path).await?;
        let mut tree: toml::Value = toml::from_str(&text)?;
        // Heuristic: any plaintext key in [[auth.keys]] marks
        // the file as v0.1.x. Also detect the absence of
        // [ratelimit] for completeness — a v0.1.x file won't
        // have it. Either condition triggers migration.
        let has_ratelimit = tree.get("ratelimit").and_then(|v| v.as_table()).is_some();
        let has_plaintext_key = tree
            .get("auth")
            .and_then(|a| a.get("keys"))
            .and_then(|k| k.as_array())
            .map(|arr| {
                arr.iter().any(|entry| {
                    entry
                        .get("key")
                        .and_then(|v| v.as_str())
                        .map(|s| !s.starts_with("enc:"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        if has_ratelimit && !has_plaintext_key {
            // Already on v0.2.0 shape.
            return Ok(());
        }
        // Backup.
        let backup = path.with_extension("toml.v0.1.bak");
        if !backup.exists() {
            tokio::fs::copy(path, &backup).await?;
            tracing::info!(from = %path.display(), to = %backup.display(),
                "v0.1.x config detected; backup written");
        } else {
            tracing::info!(backup = %backup.display(), "backup already exists; not overwriting");
        }
        // Add [ratelimit] defaults if missing.
        if !has_ratelimit {
            let mut ratelimit = toml::map::Map::new();
            ratelimit.insert("enabled".into(), toml::Value::Boolean(true));
            let mut global = toml::map::Map::new();
            global.insert("refill_per_minute".into(), toml::Value::Integer(600));
            global.insert("burst".into(), toml::Value::Integer(1200));
            let mut per_key = toml::map::Map::new();
            per_key.insert("refill_per_minute".into(), toml::Value::Integer(60));
            per_key.insert("burst".into(), toml::Value::Integer(120));
            let mut rl = toml::map::Map::new();
            rl.insert("enabled".into(), toml::Value::Boolean(true));
            rl.insert("global".into(), toml::Value::Table(global));
            rl.insert("per_key".into(), toml::Value::Table(per_key));
            tree.as_table_mut()
                .unwrap()
                .insert("ratelimit".into(), toml::Value::Table(rl));
        }
        // Encrypt auth keys if a master key is present.
        let master = crate::auth::MasterKey::from_env_strict().ok();
        if has_plaintext_key {
            if let Some(m) = master {
                if let Some(auth_table) = tree.get_mut("auth").and_then(|a| a.as_table_mut()) {
                    // Find the [[auth.keys]] entries. They can
                    // be either an array of tables (`[[auth.keys]]`)
                    // or — for serde's tolerance — a single
                    // table. Handle both shapes.
                    if let Some(arr) = auth_table.get_mut("keys").and_then(|k| k.as_array_mut()) {
                        let mut encrypted = 0usize;
                        for entry in arr.iter_mut() {
                            if let Some(table) = entry.as_table_mut() {
                                if let Some(k) = table.get("key").and_then(|v| v.as_str()) {
                                    if !k.starts_with("enc:") {
                                        let enc = m.encrypt(
                                            k,
                                            crate::auth::keystore::purpose::TOML_AUTH_KEY,
                                        );
                                        table.insert("key".into(), toml::Value::String(enc));
                                        encrypted += 1;
                                    }
                                }
                            }
                        }
                        tracing::info!(encrypted, "v0.1.x auth keys encrypted in place");
                    }
                }
            } else {
                tracing::warn!(
                    "v0.1.x config has plaintext [[auth.keys]].key values; \
                     set ROUTER_MASTER_KEY and restart to encrypt them"
                );
            }
        }
        // Persist.
        let new_text = toml::to_string_pretty(&tree)?;
        tokio::fs::write(path, new_text).await?;
        tracing::info!(path = %path.display(), "v0.1.x → v0.2.0 config rewrite complete");
        Ok(())
    }

    pub async fn snapshot(&self) -> RouterConfig {
        self.inner.read().await.clone()
    }

    /// Hot-reload. Same validation rules as `load` — refuses to
    /// swap in a config that fails the hard checks.
    pub async fn reload(&self) -> anyhow::Result<()> {
        let text = tokio::fs::read_to_string(&*self.path).await?;
        let outcome = validate_toml(&text);
        report_outcome(&outcome);
        if outcome.has_errors() {
            anyhow::bail!(
                "config at {} failed validation: {} error(s); reload aborted",
                self.path.display(),
                outcome.errors.len()
            );
        }
        let new: RouterConfig = toml::from_str(&text)?;
        let mut g = self.inner.write().await;
        *g = new;
        tracing::info!(path = %self.path.display(), "config reloaded");
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Mutate the active config in memory + persist to disk.
    /// Returns the previous snapshot so callers can diff/rollback on error.
    pub async fn update_with<F>(&self, f: F) -> anyhow::Result<RouterConfig>
    where
        F: FnOnce(&mut RouterConfig),
    {
        let prev = {
            let mut g = self.inner.write().await;
            let mut next = g.clone();
            f(&mut next);
            let _prev = g.clone();
            *g = next.clone();
            next
        };
        self.save_to_disk(&prev).await?;
        Ok(prev)
    }

    /// Write the current in-memory config to the TOML file. Useful when
    /// other code paths mutate the snapshot via `inner` and the caller
    /// wants to flush without going through `update_with`.
    pub async fn save_to_disk(&self, snapshot: &RouterConfig) -> anyhow::Result<()> {
        let serialized = toml::to_string_pretty(snapshot)?;
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await.ok();
            }
        }
        tokio::fs::write(&*self.path, serialized).await?;
        tracing::info!(path = %self.path.display(), "config saved to disk");
        Ok(())
    }
}

/// Log validator warnings at WARN; errors are not logged here
/// because the caller turns them into a startup/reload failure
/// and a more useful message is needed.
fn report_outcome(outcome: &ValidationOutcome) {
    for w in &outcome.warnings {
        tracing::warn!(path = %w.path, "{} — {}", w.path, w.reason);
    }
}
