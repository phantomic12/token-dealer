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
            let prev = g.clone();
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
