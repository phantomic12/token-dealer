//! ProviderAdapter trait + Capability enum. The trait is dyn-safe
//! without `async-trait` by returning `Pin<Box<dyn Future + Send>>`
//! — adds a small indirection but keeps the registry heterogeneous.

use crate::error::AppResult;
use crate::schema::canonical::{CanonicalChunk, CanonicalRequest, CanonicalResponse};
use futures::future::BoxFuture;
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
}
