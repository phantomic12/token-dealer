//! OpenAI Responses API adapter (/v1/responses).
//! Used for Codex, o1-pro, and deep-research models that reject
//! /v1/chat/completions. Different shape:
//!   - request: `{input: [...], model, instructions, ...}` (no `messages`)
//!   - streaming: emits `response.*` events instead of `chat.completion.chunk`
//! We translate to/from the OpenAI chat-completions shape at the boundary
//! so the rest of the router doesn't need to know.

use crate::error::AppError;
use crate::error::AppResult;
use crate::providers::adapter::{Capability, ProviderAdapter, ProviderStream};
use crate::schema::canonical::*;
use async_stream::try_stream;
use futures::StreamExt;
use http::{HeaderName, HeaderValue};
use serde_json::{json, Value};

pub struct ResponsesAdapter {
    id: String,
    base_url: String,
    default_model: String,
}

impl ResponsesAdapter {
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

impl ProviderAdapter for ResponsesAdapter {
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
            Capability::Tools => true,
            Capability::Vision => true,
            Capability::Audio => false,
            Capability::Video => false,
            Capability::Reasoning => true,
            Capability::Context(n) => n <= 200_000,
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
        // Flatten messages + system into the Responses "input" shape.
        // For MVP we treat each message as a single input item.
        let mut input: Vec<Value> = Vec::new();
        if let Some(sys) = &req.system {
            input.push(json!({"role": "system", "content": sys}));
        }
        for m in &req.messages {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => "system",
                Role::Tool => "tool",
            };
            let text: String = m
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            input.push(json!({"role": role, "content": text}));
        }
        let mut body = json!({
            "model": req.selected_model,
            "input": input,
            "stream": req.stream,
        });
        if let Some(n) = req.max_tokens {
            body["max_output_tokens"] = json!(n);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(p) = req.top_p {
            body["top_p"] = json!(p);
        }
        if let Some(tools) = &req.tools {
            body["tools"] = json!(tools
                .iter()
                .map(|t| json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }))
                .collect::<Vec<_>>());
        }
        body
    }

    fn complete<'a>(
        &'a self,
        req: &'a CanonicalRequest,
        key: &'a str,
        client: &'a reqwest::Client,
    ) -> futures::future::BoxFuture<'a, AppResult<CanonicalResponse>> {
        Box::pin(async move {
            let url = format!("{}/v1/responses", self.base_url);
            let (auth_name, auth_val) = self.auth_header(key);
            let mut body = self.build_body(req);
            if let Value::Object(ref mut m) = body {
                m.insert("stream".to_string(), Value::Bool(false));
            }
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
                return Err(AppError::ProviderError {
                    provider: self.id.clone(),
                    status: status.as_u16(),
                    message: text,
                });
            }
            let v: Value = resp.json().await?;
            parse_responses_response(&v, &self.id, &req.selected_model)
        })
    }

    fn stream<'a>(
        &'a self,
        req: &'a CanonicalRequest,
        key: &'a str,
        client: &'a reqwest::Client,
    ) -> futures::future::BoxFuture<'a, AppResult<ProviderStream>> {
        Box::pin(async move {
            let url = format!("{}/v1/responses", self.base_url);
            let (auth_name, auth_val) = self.auth_header(key);
            let mut body = self.build_body(req);
            if let Value::Object(ref mut m) = body {
                m.insert("stream".to_string(), Value::Bool(true));
            }
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
                return Err(AppError::ProviderError {
                    provider: self.id.clone(),
                    status: status.as_u16(),
                    message: text,
                });
            }
            let model_id = req.selected_model.clone();
            let message_id = format!("resp-{}", uuid::Uuid::new_v4());
            let byte_stream = resp.bytes_stream();
            let s = try_stream! {
                let mut buf: Vec<u8> = Vec::new();
                let mut acc = String::new();
                let mut last_usage: Option<Usage> = None;
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
                                let et = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
                                match et {
                                    "response.output_text.delta" => {
                                        if let Some(d) = v.get("delta").and_then(|x| x.as_str()) {
                                            acc.push_str(d);
                                            yield CanonicalChunk {
                                                id: message_id.clone(),
                                                model: model_id.clone(),
                                                delta: ContentDelta {
                                                    text: Some(d.to_string()),
                                                    tool_use: None,
                                                },
                                                finish_reason: None,
                                                usage: None,
                                            };
                                        }
                                    }
                                    "response.completed" => {
                                        if let Some(resp) = v.get("response") {
                                            if let Some(u) = resp.get("usage") {
                                                last_usage = Some(Usage {
                                                    input_tokens: u
                                                        .get("input_tokens")
                                                        .and_then(|x| x.as_u64())
                                                        .unwrap_or(0) as u32,
                                                    output_tokens: u
                                                        .get("output_tokens")
                                                        .and_then(|x| x.as_u64())
                                                        .unwrap_or(0) as u32,
                                                    cache_read_tokens: None,
                                                    cache_write_tokens: None,
                                                });
                                            }
                                            yield CanonicalChunk {
                                                id: message_id.clone(),
                                                model: model_id.clone(),
                                                delta: ContentDelta::default(),
                                                finish_reason: Some("stop".to_string()),
                                                usage: last_usage.clone(),
                                            };
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                if !acc.is_empty() && last_usage.is_none() {
                    yield CanonicalChunk {
                        id: message_id.clone(),
                        model: model_id.clone(),
                        delta: ContentDelta::default(),
                        finish_reason: Some("stop".to_string()),
                        usage: None,
                    };
                }
            };
            Ok(s.boxed())
        })
    }
}

fn parse_responses_response(
    v: &Value,
    provider_id: &str,
    fallback_model: &str,
) -> AppResult<CanonicalResponse> {
    let id = v
        .get("id")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    let model = v
        .get("model")
        .and_then(|x| x.as_str())
        .unwrap_or(fallback_model)
        .to_string();
    let mut content = Vec::new();
    if let Some(output) = v.get("output").and_then(|x| x.as_array()) {
        for item in output {
            if item.get("type").and_then(|t| t.as_str()) == Some("message") {
                if let Some(parts) = item.get("content").and_then(|c| c.as_array()) {
                    for p in parts {
                        if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
                            content.push(ContentBlock::Text {
                                text: t.to_string(),
                            });
                        }
                    }
                }
            }
        }
    }
    let usage = v.get("usage").map(|u| Usage {
        input_tokens: u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        output_tokens: u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        cache_read_tokens: None,
        cache_write_tokens: None,
    });
    Ok(CanonicalResponse {
        id,
        model,
        provider: provider_id.to_string(),
        content,
        finish_reason: Some("stop".to_string()),
        usage: usage.unwrap_or_default(),
    })
}

fn find_sse_boundary(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}
