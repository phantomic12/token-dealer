//! ConfigService — load, hold, and serve the active config under a
//! RwLock so handlers can read it lock-free on the hot path.
//! Hot-reload from disk is wired in; UI-driven reload is phase 2.

use super::types::RouterConfig;
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
    pub async fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let config = if path.exists() {
            let text = tokio::fs::read_to_string(&path).await?;
            toml::from_str::<RouterConfig>(&text)?
        } else {
            tracing::warn!(path = %path.display(), "config not found, using defaults");
            RouterConfig::default()
        };
        Ok(Self {
            inner: Arc::new(RwLock::new(config)),
            path: Arc::new(path),
        })
    }

    pub async fn snapshot(&self) -> RouterConfig {
        self.inner.read().await.clone()
    }

    pub async fn reload(&self) -> anyhow::Result<()> {
        let text = tokio::fs::read_to_string(&*self.path).await?;
        let new: RouterConfig = toml::from_str(&text)?;
        let mut g = self.inner.write().await;
        *g = new;
        tracing::info!(path = %self.path.display(), "config reloaded");
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}
