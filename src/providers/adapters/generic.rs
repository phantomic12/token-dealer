//! Generic OpenAI-compatible adapter for unknown providers.
//! Same wire format as OpenAI; capabilities are conservative.

use crate::error::{AppError, AppResult};
use crate::providers::adapter::{Capability, ProviderAdapter, ProviderStream};
use crate::schema::canonical::{CanonicalRequest, CanonicalResponse};
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::StreamExt;
use http::{HeaderName, HeaderValue};
use serde_json::Value;

pub struct GenericAdapter {
    id: String,
    base_url: String,
    default_model: String,
}

impl GenericAdapter {
    pub fn new(
        id: impl Into<String>,
        base_url: impl Into<String>,
        default_model: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into(),
            default_model: default_model.into(),
        }
    }
}

impl ProviderAdapter for GenericAdapter {
    fn provider_id(&self) -> &str {
        &self.id
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn supports(&self, cap: Capability) -> bool {
        match cap {
            // Conservative: unknown compat
            Capability::Tools => false,
            Capability::Vision => false,
            Capability::Audio => false,
            Capability::Video => false,
            Capability::Reasoning => false,
            Capability::Context(n) => n <= 8192,
        }
    }

    fn auth_header(&self, key: &str) -> (HeaderName, HeaderValue) {
        (
            HeaderName::from_static("authorization"),
            HeaderValue::from_str(&format!("Bearer {key}"))
                .unwrap_or(HeaderValue::from_static("invalid")),
        )
    }

    fn build_body(&self, req: &CanonicalRequest) -> Value {
        // Re-use OpenAI body shape (most OpenAI-compat providers use it).
        let mut messages: Vec<Value> = Vec::new();
        if let Some(sys) = &req.system {
            messages.push(serde_json::json!({"role": "system", "content": sys}));
        }
        for msg in &req.messages {
            let text: String = msg
                .content
                .iter()
                .filter_map(|b| match b {
                    super::super::super::schema::canonical::ContentBlock::Text { text } => {
                        Some(text.as_str())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            messages.push(serde_json::json!({"role": format!("{:?}", msg.role).to_lowercase(), "content": text}));
        }
        let mut body = serde_json::json!({
            "model": req.selected_model,
            "messages": messages,
            "stream": req.stream,
        });
        if let Some(n) = req.max_tokens {
            body["max_tokens"] = serde_json::json!(n);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        body
    }

    fn complete<'a>(
        &'a self,
        req: &'a CanonicalRequest,
        key: &'a str,
        client: &'a reqwest::Client,
    ) -> BoxFuture<'a, AppResult<CanonicalResponse>> {
        Box::pin(async move {
            let url = format!("{}/v1/chat/completions", self.base_url);
            let (auth_name, auth_val) = self.auth_header(key);
            let mut body = self.build_body(req);
            if let Value::Object(ref mut m) = body {
                m.insert("stream".to_string(), Value::Bool(false));
            }
            let resp = client
                .post(&url)
                .header(auth_name, auth_val)
                .json(&body)
                .send()
                .await?;
            let status = resp.status();
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(AppError::ProviderError {
                    provider: self.id.clone(),
                    status: status.as_u16(),
                    message: text,
                });
            }
            let v: Value = resp.json().await?;
            // Try to parse OpenAI-shape response; fall back to a minimal envelope.
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or_default().to_string();
            let model = v
                .get("model")
                .and_then(|x| x.as_str())
                .unwrap_or(&req.selected_model)
                .to_string();
            let content = v
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|x| x.as_str())
                .map(|s| {
                    vec![super::super::super::schema::canonical::ContentBlock::Text {
                        text: s.to_string(),
                    }]
                })
                .unwrap_or_default();
            let finish_reason = v
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
                .and_then(|c| c.get("finish_reason"))
                .and_then(|x| x.as_str())
                .map(String::from);
            let usage = v
                .get("usage")
                .map(|u| super::super::super::schema::canonical::Usage {
                    input_tokens: u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                    output_tokens: u.get("completion_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                })
                .unwrap_or_default();
            Ok(CanonicalResponse {
                id,
                model,
                provider: self.id.clone(),
                content,
                finish_reason,
                usage,
            })
        })
    }

    fn stream<'a>(
        &'a self,
        _req: &'a CanonicalRequest,
        _key: &'a str,
        _client: &'a reqwest::Client,
    ) -> BoxFuture<'a, AppResult<ProviderStream>> {
        // Generic adapter is a non-streaming fallback. For the MVP we
        // simply return an empty stream — full SSE is wired in the
        // OpenAI adapter and a real generic provider should be modeled
        // explicitly if streaming is needed.
        Box::pin(async move { Ok(futures::stream::empty().boxed()) })
    }
}
