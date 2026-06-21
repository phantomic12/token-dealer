//! Telemetry — OpenTelemetry traces + metrics export.
//!
//! Optional. Activated when `OTEL_EXPORTER_OTLP_ENDPOINT` is set in
//! the environment (standard OTLP env var). Otherwise the no-op
//! exporter is used and tracing falls back to the local
//! `tracing-subscriber` setup.
//!
//! Span structure:
//!   td.request
//!     ├─ td.auth (resolves user from Bearer/cookie)
//!     ├─ td.route (selector picks provider/model)
//!     ├─ td.upstream (provider API call)
//!     └─ td.cost (records input/output tokens + USD)
//!
//! Per-span attributes: `td.user_id`, `td.user_role`,
//! `td.provider_id`, `td.model_id`, `td.input_tokens`,
//! `td.output_tokens`, `td.cost_usd`, `td.request_id`.

use opentelemetry::global;
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::trace::TracerProvider;
use opentelemetry_sdk::Resource;
use opentelemetry_semantic_conventions::resource::SERVICE_NAME;
use std::sync::Arc;

#[derive(Clone)]
pub struct Telemetry {
    inner: Arc<TelemetryInner>,
}

struct TelemetryInner {
    /// None when OTLP is disabled (no endpoint configured).
    provider: Option<TracerProvider>,
}

impl Telemetry {
    pub fn init() -> Self {
        let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
            .ok()
            .filter(|s| !s.is_empty());
        let provider = endpoint.and_then(|ep| {
            let url = ep.clone();
            let exporter = opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(ep)
                .build()
                .ok()?;
            let resource = Resource::new(vec![KeyValue::new(SERVICE_NAME, "token-dealer")]);
            let p = TracerProvider::builder()
                .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
                .with_resource(resource)
                .build();
            global::set_tracer_provider(p.clone());
            tracing::info!("OTLP tracing enabled, exporting to {}", url);
            Some(p)
        });
        Self {
            inner: Arc::new(TelemetryInner { provider }),
        }
    }

    pub fn enabled(&self) -> bool {
        self.inner.provider.is_some()
    }

    /// Force-flush all pending spans. Called before shutdown.
    pub fn shutdown(&self) {
        if let Some(p) = &self.inner.provider {
            let _ = p.shutdown();
        }
    }
}
