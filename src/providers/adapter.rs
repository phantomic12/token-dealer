//! ProviderAdapter trait + Capability enum. The trait is dyn-safe
//! without `async-trait` by returning `Pin<Box<dyn Future + Send>>`
//! — adds a small indirection but keeps the registry heterogeneous.

use crate::error::{AppError, AppResult};
use crate::schema::canonical::{CanonicalChunk, CanonicalRequest, CanonicalResponse};
use futures::future::{BoxFuture, FutureExt};
use futures::stream::BoxStream;
use http::{HeaderName, HeaderValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    Tools,
    Vision,
    Audio,
    Video,
    Reasoning,
    /// Adapter claims to handle at least this many input tokens.
    Context(u32),
}

pub type ProviderStream = BoxStream<'static, AppResult<CanonicalChunk>>;

pub trait ProviderAdapter: Send + Sync {
    fn provider_id(&self) -> &str;

    fn base_url(&self) -> &str;

    fn default_model(&self) -> &str;

    fn supports(&self, cap: Capability) -> bool;

    /// Build the auth header for this provider (Bearer for OpenAI-style,
    /// x-api-key for Anthropic, etc.).
    fn auth_header(&self, key: &str) -> (HeaderName, HeaderValue);

    /// Build the provider-specific request body from a canonical one.
    fn build_body(&self, req: &CanonicalRequest) -> serde_json::Value;

    /// Non-streaming completion.
    fn complete<'a>(
        &'a self,
        req: &'a CanonicalRequest,
        key: &'a str,
        client: &'a reqwest::Client,
    ) -> BoxFuture<'a, AppResult<CanonicalResponse>>;

    /// Streaming completion. Returns a stream of canonical chunks.
    fn stream<'a>(
        &'a self,
        req: &'a CanonicalRequest,
        key: &'a str,
        client: &'a reqwest::Client,
    ) -> BoxFuture<'a, AppResult<ProviderStream>>;

    /// Live model list. Default hits the OpenAI-compat `/v1/models`
    /// endpoint with `Authorization: Bearer <key>`. Adapters with a
    /// different shape (Anthropic, Google) override this.
    fn list_models<'a>(
        &'a self,
        key: &'a str,
        client: &'a reqwest::Client,
    ) -> BoxFuture<'a, AppResult<Vec<String>>> {
        async move {
            let url = format!("{}/v1/models", self.base_url().trim_end_matches('/'));
            let resp = client
                .get(&url)
                .header("authorization", format!("Bearer {key}"))
                .send()
                .await
                .map_err(|e| AppError::Internal(format!("models list: {e}")))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(AppError::Internal(format!(
                    "models list {} {}: {}",
                    status.as_u16(),
                    status.canonical_reason().unwrap_or(""),
                    text.chars().take(200).collect::<String>()
                )));
            }
            let v: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| AppError::Internal(format!("models parse: {e}")))?;
            // OpenAI shape: { "data": [{"id": "...", ...}, ...] }
            let models = v
                .get("data")
                .and_then(|d| d.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|m| m.get("id").and_then(|i| i.as_str()))
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            Ok(models)
        }
        .boxed()
    }
}
