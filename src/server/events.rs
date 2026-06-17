//! Server-Sent Events stream. Mirrors mnfst/manifest's `/api/v1/events`
//! endpoint — pushes real-time updates to the WebUI as requests flow
//! through. Events emitted:
//!
//!   - `request.completed`: `{ request_id, tier, provider, model, cost,
//!     input_tokens, output_tokens, latency_ms, finished_at }`
//!   - `budget.warning`:    `{ user_id, kind, fraction }`
//!   - `pricing.synced`:    `{ upserted, url }`
//!
//! In-memory broadcast: each subscriber gets its own unbounded
//! channel; slow subscribers fall behind and the channel fills.
//! Production deployments behind many tabs should set a tighter
//! `EventBus::subscribe` and rate-limit the UI.

use crate::AppState;
use axum::{
    extract::State,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
};
use serde::Serialize;
use serde_json::Value;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Event payload broadcast to all subscribers.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RouterEvent {
    RequestCompleted {
        request_id: String,
        tier: String,
        provider: String,
        model: String,
        cost_usd: Option<f64>,
        input_tokens: Option<u32>,
        output_tokens: Option<u32>,
        latency_ms: u64,
        finished_at: i64,
    },
    BudgetWarning {
        user_id: Option<String>,
        kind: String,
        fraction: f64,
    },
    PricingSynced {
        upserted: usize,
        url: String,
    },
}

/// In-process broadcast bus. Clone-able; clones share the same
/// underlying channel.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Value>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Emit a typed event. JSON-serializes to the SSE `data` field.
    pub fn emit(&self, event: &RouterEvent) {
        let payload = serde_json::to_value(event).unwrap_or(Value::Null);
        let _ = self.tx.send(payload);
    }

    /// Subscribe to all events. Returns a Receiver that yields
    /// serialized JSON values.
    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(1024)
    }
}

/// Shared, lazy-init event bus held on AppState.
pub type SharedEventBus = Arc<EventBus>;

/// SSE handler — `GET /api/v1/events`.
pub async fn sse_events(State(state): State<AppState>) -> impl IntoResponse {
    // Lazily create the bus if absent. In a long-running process this
    // is allocated once.
    let bus = state.events.clone();
    let stream = async_stream::stream! {
        let mut rx = bus.subscribe();
        loop {
            match rx.recv().await {
                Ok(payload) => {
                    let event_name = payload.get("type").and_then(|v| v.as_str()).unwrap_or("message");
                    yield Ok::<_, Infallible>(
                        Event::default()
                            .event(event_name)
                            .data(payload.to_string()),
                    );
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Subscriber fell behind. Skip + continue.
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::new())
}
