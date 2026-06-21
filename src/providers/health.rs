//! Per-provider health state. Backed by an in-memory map for the MVP;
//! persistence (survive restarts) is a phase-2 concern.

use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderHealthState {
    Healthy,
    Degraded,
    Down,
}

#[derive(Debug, Clone)]
pub struct ProviderHealth {
    pub status: ProviderHealthState,
    pub consecutive_failures: u32,
    pub cooldown_until: Option<std::time::Instant>,
    pub last_failure: Option<std::time::Instant>,
}

impl Default for ProviderHealth {
    fn default() -> Self {
        Self {
            status: ProviderHealthState::Healthy,
            consecutive_failures: 0,
            cooldown_until: None,
            last_failure: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct HealthRegistry {
    inner: Arc<RwLock<std::collections::HashMap<String, ProviderHealth>>>,
}

impl HealthRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get(&self, provider_id: &str) -> ProviderHealth {
        self.inner
            .read()
            .await
            .get(provider_id)
            .cloned()
            .unwrap_or_default()
    }

    pub async fn record_success(&self, provider_id: &str) {
        let mut g = self.inner.write().await;
        g.entry(provider_id.to_string()).or_default().status = ProviderHealthState::Healthy;
        g.entry(provider_id.to_string())
            .or_default()
            .consecutive_failures = 0;
        g.entry(provider_id.to_string()).or_default().cooldown_until = None;
    }

    pub async fn record_failure(&self, provider_id: &str, threshold: u32, cooldown_secs: u64) {
        let mut g = self.inner.write().await;
        let entry = g.entry(provider_id.to_string()).or_default();
        entry.consecutive_failures += 1;
        entry.last_failure = Some(std::time::Instant::now());
        if entry.consecutive_failures >= threshold {
            entry.status = ProviderHealthState::Down;
            entry.cooldown_until =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(cooldown_secs));
        }
    }

    pub async fn is_available(&self, provider_id: &str) -> bool {
        let g = self.inner.read().await;
        let h = match g.get(provider_id) {
            None => return true,
            Some(h) => h,
        };
        !matches!(h.cooldown_until, Some(until) if until > std::time::Instant::now())
    }
}
