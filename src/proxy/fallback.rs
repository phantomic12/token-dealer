//! Fallback chain execution + retry classification.
//!
//! Error classification (per design doc):
//!   RETRY same provider (max 1):  429, 408
//!   SKIP to next fallback:        500/502/503/504, 529, connect/read timeout
//!   FATAL — stop chain:           401, 403 (bad key, won't self-heal)
//!   CONTEXT — find higher-context: 400 with context-too-long body
//!
//! The non-streaming path buffers the full response, so we can fall
//! back transparently. The streaming path is "commit to primary" —
//! retries are still possible (the request hasn't reached the client
//! yet on the first attempt), but mid-stream fallbacks are not.

use super::super::error::AppError;
use super::super::providers::health::HealthRegistry;
use super::super::providers::ProviderAdapter;
use super::super::schema::canonical::{CanonicalRequest, CanonicalResponse};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Per-tier routing plan: primary + ordered fallback list + optional
/// downgrade tier.
#[derive(Debug, Clone)]
pub struct RoutingPlan {
    pub request: CanonicalRequest,
    pub primary: String,         // "provider/model"
    pub fallbacks: Vec<String>,  // "provider/model" list
    pub downgrade_to: Option<String>,
    pub request_budget: Duration,
    pub max_retries_per_provider: u32,
    pub max_retry_after_ms: u64,
    pub fixed_retry_wait_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptOutcome {
    Success,
    /// Transient — retry same provider once.
    Retry { wait_ms: u64, reason: &'static str },
    /// Provider failed — try next fallback.
    Skip { reason: &'static str },
    /// 401/403 — provider is poisoned, remove from chain.
    Fatal { reason: &'static str },
    /// 400 with context-too-long — find a model with a bigger window.
    ContextTooLong { limit: u32 },
}

impl AttemptOutcome {
    pub fn is_retry(&self) -> bool {
        matches!(self, AttemptOutcome::Retry { .. })
    }
    pub fn is_skip(&self) -> bool {
        matches!(self, AttemptOutcome::Skip { .. })
    }
    pub fn is_fatal(&self) -> bool {
        matches!(self, AttemptOutcome::Fatal { .. })
    }
    pub fn is_context(&self) -> bool {
        matches!(self, AttemptOutcome::ContextTooLong { .. })
    }
}

pub fn classify(err: &AppError) -> AttemptOutcome {
    match err {
        AppError::ProviderError { status, message, .. } => {
            classify_status(*status, message)
        }
        AppError::UpstreamTimeout { .. } => AttemptOutcome::Skip {
            reason: "upstream timeout",
        },
        AppError::ContextTooLong { limit, .. } => AttemptOutcome::ContextTooLong {
            limit: *limit,
        },
        AppError::Internal(_) => AttemptOutcome::Skip {
            reason: "internal error",
        },
        _ => AttemptOutcome::Skip {
            reason: "unknown error",
        },
    }
}

fn classify_status(status: u16, message: &str) -> AttemptOutcome {
    let lower = message.to_lowercase();
    match status {
        401 | 403 => AttemptOutcome::Fatal {
            reason: "auth error",
        },
        408 => AttemptOutcome::Retry {
            wait_ms: 0,
            reason: "request timeout",
        },
        429 => AttemptOutcome::Retry {
            wait_ms: 1500,
            reason: "rate limited",
        },
        400 if lower.contains("context")
            || lower.contains("too long")
            || lower.contains("maximum context") =>
        {
            AttemptOutcome::ContextTooLong { limit: 8192 }
        }
        500 | 502 | 503 | 504 | 529 => AttemptOutcome::Skip {
            reason: "upstream error",
        },
        _ => AttemptOutcome::Skip {
            reason: "upstream error",
        },
    }
}

#[derive(Debug, Clone)]
pub struct AttemptRecord {
    pub provider: String,
    pub model: String,
    pub outcome: AttemptOutcome,
    pub latency_ms: i64,
}

#[derive(Debug)]
pub struct ExecutionResult {
    pub response: CanonicalResponse,
    pub attempts: Vec<AttemptRecord>,
    pub providers_tried: Vec<String>,
    pub fallback_count: u32,
}

/// What the executor needs to actually talk to a provider.
pub struct ProviderHandle {
    pub provider_id: String,
    pub model_id: String,
    pub adapter: Arc<dyn ProviderAdapter>,
    pub key: String,
}

/// What the executor needs to track per-provider health (the
/// circuit breaker). The caller owns the registry; the executor
/// reads availability and records success/failure.
pub struct HealthHook {
    pub registry: HealthRegistry,
    pub failure_threshold: u32,
    pub cooldown_secs: u64,
}

/// Execute a routing plan against a resolver + health hook. The
/// resolver returns a `ProviderHandle` (adapter + key) for a given
/// provider id. The health hook short-circuits providers in cooldown
/// and records outcomes so the next call can probe.
pub async fn execute<F, Fut>(
    plan: RoutingPlan,
    resolve: F,
    health: &HealthHook,
) -> Result<ExecutionResult, AppError>
where
    F: Fn(&str) -> Fut,
    Fut: std::future::Future<Output = Option<ProviderHandle>>,
{
    let started = Instant::now();
    let mut attempts: Vec<AttemptRecord> = Vec::new();
    let mut providers_tried: Vec<String> = Vec::new();
    let mut fallback_count: u32 = 0;

    let mut chain: Vec<(String, String)> = vec![split_provider_model(&plan.primary)];
    for fb in &plan.fallbacks {
        chain.push(split_provider_model(fb));
    }

    for (provider_id, model_id) in chain {
        if started.elapsed() >= plan.request_budget {
            return Err(AppError::Internal(format!(
                "request budget exhausted after {}ms",
                started.elapsed().as_millis()
            )));
        }

        // Circuit breaker: skip if this provider is in cooldown.
        if !health.registry.is_available(&provider_id).await {
            attempts.push(AttemptRecord {
                provider: provider_id.clone(),
                model: model_id.clone(),
                outcome: AttemptOutcome::Skip {
                    reason: "in cooldown",
                },
                latency_ms: 0,
            });
            providers_tried.push(provider_id);
            fallback_count += 1;
            continue;
        }

        let Some(handle) = resolve(&provider_id).await else {
            attempts.push(AttemptRecord {
                provider: provider_id.clone(),
                model: model_id.clone(),
                outcome: AttemptOutcome::Skip { reason: "no adapter" },
                latency_ms: 0,
            });
            providers_tried.push(provider_id);
            fallback_count += 1;
            continue;
        };

        providers_tried.push(handle.provider_id.clone());

        let mut canonical = plan.request.clone();
        canonical.selected_provider = handle.provider_id.clone();
        canonical.selected_model = handle.model_id.clone();

        // First attempt
        let attempt_started = Instant::now();
        let first = handle
            .adapter
            .complete(&canonical, &handle.key, &reqwest::Client::new())
            .await;
        let first_latency = attempt_started.elapsed().as_millis() as i64;

        let outcome = match &first {
            Ok(_) => AttemptOutcome::Success,
            Err(e) => classify(e),
        };

        // Record outcome on health
        if outcome.is_retry() || matches!(outcome, AttemptOutcome::Success) {
            // success; reset failure count
            health
                .registry
                .record_success(&handle.provider_id)
                .await;
        } else {
            health
                .registry
                .record_failure(
                    &handle.provider_id,
                    health.failure_threshold,
                    health.cooldown_secs,
                )
                .await;
        }

        attempts.push(AttemptRecord {
            provider: handle.provider_id.clone(),
            model: handle.model_id.clone(),
            outcome: outcome.clone(),
            latency_ms: first_latency,
        });

        if let Ok(resp) = first {
            return Ok(ExecutionResult {
                response: resp,
                attempts,
                providers_tried,
                fallback_count,
            });
        }

        // One retry if Retry
        let mut final_attempt = first;
        if outcome.is_retry() {
            let wait_ms = match outcome {
                AttemptOutcome::Retry { wait_ms, .. } => wait_ms.min(plan.max_retry_after_ms),
                _ => 0,
            };
            if wait_ms > 0 {
                tokio::time::sleep(Duration::from_millis(wait_ms)).await;
            }
            let retry_started = Instant::now();
            let retry = handle
                .adapter
                .complete(&canonical, &handle.key, &reqwest::Client::new())
                .await;
            let retry_latency = retry_started.elapsed().as_millis() as i64;
            let retry_outcome = match &retry {
                Ok(_) => AttemptOutcome::Success,
                Err(e) => classify(e),
            };
            if retry_outcome.is_retry() || matches!(retry_outcome, AttemptOutcome::Success) {
                health
                    .registry
                    .record_success(&handle.provider_id)
                    .await;
            } else {
                health
                    .registry
                    .record_failure(
                        &handle.provider_id,
                        health.failure_threshold,
                        health.cooldown_secs,
                    )
                    .await;
            }
            attempts.push(AttemptRecord {
                provider: handle.provider_id.clone(),
                model: handle.model_id.clone(),
                outcome: retry_outcome.clone(),
                latency_ms: retry_latency,
            });
            if let Ok(resp) = retry {
                return Ok(ExecutionResult {
                    response: resp,
                    attempts,
                    providers_tried,
                    fallback_count,
                });
            }
            final_attempt = retry;
        }

        if let AttemptOutcome::Fatal { .. } = outcome {
            return Err(final_attempt.unwrap_err());
        }
        if let AttemptOutcome::ContextTooLong { limit } = outcome {
            return Err(AppError::ContextTooLong {
                model: model_id.clone(),
                limit,
            });
        }

        // Skip to next fallback.
        fallback_count += 1;
    }

    Err(AppError::Internal(format!(
        "all fallbacks exhausted ({} tried)",
        providers_tried.len()
    )))
}

fn split_provider_model(s: &str) -> (String, String) {
    match s.split_once('/') {
        Some((p, m)) => (p.to_string(), m.to_string()),
        None => ("unknown".to_string(), s.to_string()),
    }
}
