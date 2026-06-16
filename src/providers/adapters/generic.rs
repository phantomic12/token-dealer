//! Generic OpenAI-compatible adapter for unknown providers.
//! Wraps `OpenAiAdapter` so streaming and all the other niceties come
//! for free. Capabilities are conservative.

use crate::error::AppResult;
use crate::providers::adapter::{Capability, ProviderAdapter, ProviderStream};
use crate::providers::adapters::openai::OpenAiAdapter;
use crate::schema::canonical::{CanonicalRequest, CanonicalResponse};
use async_stream::try_stream;
use futures::StreamExt;
use http::{HeaderName, HeaderValue};
use serde_json::Value;

pub struct GenericAdapter {
    id: String,
    base_url: String,
    path: String,
    default_model: String,
}

impl GenericAdapter {
    pub fn new(
        id: impl Into<String>,
        base_url: impl Into<String>,
        default_model: impl Into<String>,
    ) -> Self {
        Self::with_path(id, base_url, "/v1/chat/completions".to_string(), default_model)
    }

    pub fn with_path(
        id: impl Into<String>,
        base_url: impl Into<String>,
        path: String,
        default_model: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into(),
            path,
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
        // Conservative: unknown compat
        match cap {
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

    /// Build a body for chat-style endpoints. Subclasses (or callers)
    /// can override for image/audio/video paths.
    fn build_body(&self, req: &CanonicalRequest) -> Value {
        let mut messages: Vec<Value> = Vec::new();
        if let Some(sys) = &req.system {
            messages.push(serde_json::json!({"role": "system", "content": sys}));
        }
        for msg in &req.messages {
            let text: String = msg
                .content
                .iter()
                .filter_map(|b| match b {
                    crate::schema::canonical::ContentBlock::Text { text } => {
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
    ) -> futures::future::BoxFuture<'a, AppResult<CanonicalResponse>> {
        let url = format!("{}{}", self.base_url.trim_end_matches('/'), self.path);
        let (auth_name, auth_val) = self.auth_header(key);
        let mut body = self.build_body(req);
        if let Value::Object(ref mut m) = body {
            m.insert("stream".to_string(), Value::Bool(false));
        }
        Box::pin(async move {
            let resp = client
                .post(&url)
                .header(auth_name, auth_val)
                .json(&body)
                .send()
                .await?;
            let status = resp.status();
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(crate::error::AppError::ProviderError {
                    provider: self.id.clone(),
                    status: status.as_u16(),
                    message: text,
                });
            }
            let v: Value = resp.json().await?;
            // Try to parse OpenAI-shape response; fall back to a minimal envelope.
            let id = v
                .get("id")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
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
                    vec![crate::schema::canonical::ContentBlock::Text {
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
                .map(|u| crate::schema::canonical::Usage {
                    input_tokens: u
                        .get("prompt_tokens")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0) as u32,
                    output_tokens: u
                        .get("completion_tokens")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0) as u32,
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

    /// Streaming. We assume OpenAI-compat SSE (`data: {json}\n\n`,
    /// terminated by `data: [DONE]\n\n`). Parse with the same logic
    /// the OpenAI adapter uses, but route through this adapter's
    /// base URL + path + auth header.
    fn stream<'a>(
        &'a self,
        req: &'a CanonicalRequest,
        key: &'a str,
        client: &'a reqwest::Client,
    ) -> futures::future::BoxFuture<'a, AppResult<ProviderStream>> {
        let url = format!("{}{}", self.base_url.trim_end_matches('/'), self.path);
        let (auth_name, auth_val) = self.auth_header(key);
        let mut body = self.build_body(req);
        if let Value::Object(ref mut m) = body {
            m.insert("stream".to_string(), Value::Bool(true));
            m.insert("stream_options".to_string(), serde_json::json!({"include_usage": true}),);
        }

        Box::pin(async move {
            let resp = client
                .post(&url)
                .header(auth_name, auth_val)
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await?;
            let status = resp.status();
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(crate::error::AppError::ProviderError {
                    provider: self.id.clone(),
                    status: status.as_u16(),
                    message: text,
                });
            }
            let model_id = req.selected_model.clone();
            let message_id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
            let byte_stream = resp.bytes_stream();
            let s = try_stream! {
                let mut buf: Vec<u8> = Vec::new();
                tokio::pin!(byte_stream);
                while let Some(chunk) = byte_stream.next().await {
                    let chunk = chunk?;
                    buf.extend_from_slice(&chunk);
                    while let Some(pos) = find_sse_boundary(&buf) {
                        let raw: Vec<u8> = buf.drain(..pos).collect();
                        let _ = buf.drain(..2);
                        let text = String::from_utf8_lossy(&raw);
                        for line in text.lines() {
                            let data = match line.strip_prefix("data: ") {
                                Some(d) => d,
                                None => continue,
                            };
                            if data == "[DONE]" {
                                continue;
                            }
                            if let Ok(v) = serde_json::from_str::<Value>(data) {
                                if let Some(parsed) = parse_openai_compat_chunk(&v, &model_id) {
                                    let mut c = parsed;
                                    if c.id.is_empty() {
                                        c.id = message_id.clone();
                                    }
                                    yield c;
                                }
                            }
                        }
                    }
                }
            };
            Ok(s.boxed())
        })
    }
}

fn find_sse_boundary(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}

/// Parse a single OpenAI-compat SSE chunk into a CanonicalChunk.
/// Used by the generic streaming path — no model_id stamping, no
/// provider_id stamp. The caller fills those in.
fn parse_openai_compat_chunk(v: &Value, model_id: &str) -> Option<crate::schema::canonical::CanonicalChunk> {
    use crate::schema::canonical::{CanonicalChunk, ContentDelta};
    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let choice = v.get("choices")?.as_array()?.first()?;
    let delta = choice.get("delta")?;
    let text = delta
        .get("content")
        .and_then(|x| x.as_str())
        .map(String::from);
    let tool_use = delta
        .get("tool_calls")
        .and_then(|x| x.as_array())
        .and_then(|arr| arr.first())
        .map(|tc| crate::schema::canonical::CanonicalToolCall {
            id: tc.get("id").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
            name: tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string(),
            arguments: tc
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(|x| x.as_str())
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(Value::Null),
        });
    let finish_reason = choice
        .get("finish_reason")
        .and_then(|x| x.as_str())
        .map(String::from);
    let usage = v.get("usage").map(|u| crate::schema::canonical::Usage {
        input_tokens: u
            .get("prompt_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32,
        output_tokens: u
            .get("completion_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32,
        cache_read_tokens: None,
        cache_write_tokens: None,
    });
    Some(CanonicalChunk {
        id,
        model: model_id.to_string(),
        delta: ContentDelta { text, tool_use },
        finish_reason,
        usage,
    })
}
