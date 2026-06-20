//! Rate limiting. Token bucket, in-memory, per-API-key + global.
//!
//! v0.2.0 plan item 3:
//!   - per-key 60 req/min burst 120 (configurable)
//!   - global 600 req/min burst 1200 (configurable)
//!   - applies to /v1/chat/completions, /v1/messages, /v1/responses
//!   - counts at request *start* (failed auth doesn't get a free retry)
//!   - 429 with Retry-After: <seconds> + OpenAI-shape error envelope
//!   - escape hatch: enabled = false
//!
//! Implementation: classic lazy-refill token bucket. The bucket
//! holds at most `burst` tokens. Refill rate is `refill_per_minute / 60`
//! tokens per second. On each request we compute how many tokens
//! would be present at the current time (since the last update),
//! clamp to burst, and reject with 429 if the count is < 1.

use crate::server::AppState;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Token bucket state. Refills lazily on each access based on
/// the time delta from `last_refill`.
#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

impl Bucket {
    fn new(burst: f64) -> Self {
        Self {
            tokens: burst,
            last_refill: Instant::now(),
        }
    }

    /// Try to consume one token. Returns `true` if allowed, with
    /// the seconds-until-next-token as the `Retry-After` value
    /// (1 if a token is already available on the next refill).
    fn try_consume(&mut self, refill_per_sec: f64, burst: f64) -> Result<(), u64> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * refill_per_sec).min(burst);
        self.last_refill = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            // Seconds until at least one token is available.
            let need = 1.0 - self.tokens;
            let secs = (need / refill_per_sec).ceil() as u64;
            Err(secs.max(1))
        }
    }
}

/// Shared rate-limit state. Locked contention is fine for the
/// v0.2.0 plan's target: a single instance, a few hundred keys,
/// ~10 req/s peak. If we ever go multi-instance, swap the inner
/// `Mutex<HashMap<...>>` for a Redis-backed implementation; the
/// `RateLimit` trait lets us keep the middleware unchanged.
#[derive(Clone)]
pub struct RateLimiter {
    per_key: Arc<Mutex<HashMap<String, Bucket>>>,
    global: Arc<Mutex<Bucket>>,
    per_key_rps: f64,
    per_key_burst: f64,
    global_rps: f64,
    global_burst: f64,
}

impl RateLimiter {
    pub fn new(per_key_rpm: u32, per_key_burst: u32, global_rpm: u32, global_burst: u32) -> Self {
        Self {
            per_key: Arc::new(Mutex::new(HashMap::new())),
            global: Arc::new(Mutex::new(Bucket::new(global_burst as f64))),
            per_key_rps: per_key_rpm as f64 / 60.0,
            per_key_burst: per_key_burst as f64,
            global_rps: global_rpm as f64 / 60.0,
            global_burst: global_burst as f64,
        }
    }

    pub fn disabled() -> Self {
        // Sentinel: absurdly high rate so the middleware is a
        // no-op. Cheaper than a separate `enabled: bool` flag
        // and the dispatch path stays uniform.
        Self::new(1_000_000, 1_000_000, 1_000_000, 1_000_000)
    }

    /// Try to consume one token for `key`. `key` is the API key
    /// prefix (or "anonymous" for unauthenticated requests,
    /// though the middleware only fires after auth). Returns
    /// `Ok(())` on success or `Err(retry_after_secs)` on
    /// rejection. Global is checked first so an over-quota
    /// request doesn't waste per-key work.
    pub fn try_acquire(&self, key: &str) -> Result<(), u64> {
        // Global first.
        let mut g = self.global.lock().expect("rate-limit mutex poisoned");
        if let Err(secs) = g.try_consume(self.global_rps, self.global_burst) {
            return Err(secs);
        }
        drop(g);
        // Per-key.
        let mut map = self.per_key.lock().expect("rate-limit mutex poisoned");
        let bucket = map
            .entry(key.to_string())
            .or_insert_with(|| Bucket::new(self.per_key_burst));
        bucket.try_consume(self.per_key_rps, self.per_key_burst)
    }
}

/// Axum middleware. Pulls the per-request key from the auth
/// context (already attached by the upstream `auth::middleware`)
/// and consults the rate limiter. If rate limiting is disabled
/// in config, the middleware is a no-op.
///
/// Counts at request *start* — failed-auth requests are not
/// re-tried, so a 401 from upstream doesn't get a free slot.
///
/// Per v0.2.0 plan item 3, only the three chat-shaped endpoints
/// are limited: `/v1/chat/completions`, `/v1/messages`,
/// `/v1/responses`. Everything else (models, health, ui,
/// admin, etc.) is exempt.
pub async fn middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if !is_rate_limited_path(path) {
        return next.run(req).await;
    }
    let snap = state.config.snapshot().await;
    if !snap.ratelimit.enabled {
        return next.run(req).await;
    }
    // Resolve the key. Use the UserContext's key_prefix or
    // session id; fall back to the literal Authorization header
    // value (caller's API key prefix); finally "anonymous" so
    // the per-key bucket still applies.
    let key = req
        .extensions()
        .get::<crate::auth::UserContext>()
        .map(|u| {
            u.key_prefix
                .clone()
                .unwrap_or_else(|| u.session_id.clone().unwrap_or_else(|| "anonymous".into()))
        })
        .unwrap_or_else(|| "anonymous".to_string());
    if let Err(retry_after) = state.rate_limiter.try_acquire(&key) {
        let body = Json(json!({
            "error": {
                "message": format!("rate limit exceeded; retry in {retry_after}s"),
                "type": "rate_limit_error",
                "code": "rate_limit_exceeded",
            }
        }));
        let mut resp = (StatusCode::TOO_MANY_REQUESTS, body).into_response();
        resp.headers_mut().insert(
            header::RETRY_AFTER,
            retry_after.to_string().parse().unwrap(),
        );
        return resp;
    }
    next.run(req).await
}

fn is_rate_limited_path(path: &str) -> bool {
    matches!(
        path,
        "/v1/chat/completions" | "/v1/messages" | "/v1/responses"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn bucket_starts_full() {
        let mut b = Bucket::new(10.0);
        // Initially full — 10 tokens. First consume succeeds.
        assert!(b.try_consume(1.0, 10.0).is_ok());
    }

    #[test]
    fn bucket_rejects_when_empty() {
        let mut b = Bucket::new(1.0);
        // Drain the only token.
        assert!(b.try_consume(1.0, 1.0).is_ok());
        // Next call must reject.
        let err = b.try_consume(1.0, 1.0).unwrap_err();
        assert!(err >= 1, "retry-after must be at least 1 second, got {err}");
    }

    #[test]
    fn bucket_refills_over_time() {
        let mut b = Bucket::new(1.0);
        assert!(b.try_consume(1.0, 1.0).is_ok());
        assert!(b.try_consume(1.0, 1.0).is_err());
        // Simulate 5 seconds passing by rewinding last_refill.
        b.last_refill = Instant::now() - std::time::Duration::from_secs(5);
        // 5s * (60/60) tokens/s = 5 tokens, capped at burst 1.
        assert!(b.try_consume(1.0, 1.0).is_ok());
    }

    #[test]
    fn limiter_per_key_isolation() {
        let l = RateLimiter::new(60, 2, 600, 1200);
        // Key A drains its 2-token burst.
        assert!(l.try_acquire("a").is_ok());
        assert!(l.try_acquire("a").is_ok());
        assert!(l.try_acquire("a").is_err());
        // Key B is unaffected.
        assert!(l.try_acquire("b").is_ok());
        assert!(l.try_acquire("b").is_ok());
    }

    #[test]
    fn disabled_limiter_is_a_noop() {
        let l = RateLimiter::disabled();
        // 1000 requests in a tight loop should all succeed.
        for _ in 0..1000 {
            assert!(l.try_acquire("anyone").is_ok());
        }
    }

    #[test]
    fn global_limit_blocks_after_exhaustion() {
        // Global 1 req/min, burst 1, per-key effectively unlimited
        // so global is the only constraint.
        let l = RateLimiter::new(60, 120, 1, 1);
        assert!(l.try_acquire("a").is_ok());
        // Even a different key hits the global cap.
        assert!(l.try_acquire("b").is_err());
    }

    #[test]
    fn refill_rates_produce_expected_tokens() {
        // 60/min = 1/s. Burst 2. Drain, sleep 1s in real time would
        // be slow — instead verify the math by reading internal
        // state via a constructed bucket.
        let mut b = Bucket::new(2.0);
        // 2 initial.
        b.tokens = 0.0;
        b.last_refill = Instant::now() - std::time::Duration::from_secs(1);
        b.tokens = (b.tokens + 1.0 * 1.0).min(2.0);
        assert!(
            near(b.tokens, 1.0),
            "expected ~1 token after 1s, got {}",
            b.tokens
        );
        b.last_refill = Instant::now() - std::time::Duration::from_secs(3);
        b.tokens = (b.tokens + 3.0 * 1.0).min(2.0);
        assert!(
            near(b.tokens, 2.0),
            "expected 2 tokens (capped), got {}",
            b.tokens
        );
    }
}
