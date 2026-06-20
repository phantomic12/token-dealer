//! Anthropic Messages API adapter. Diverges from OpenAI on:
//!   - endpoint: POST /v1/messages (not /chat/completions)
//!   - auth: x-api-key header (not Authorization: Bearer)
//!   - version header required
//!   - system prompt at top level (not in messages)
//!   - tool calls are content blocks (not message-level tool_calls)
//!   - streaming: event types (message_start, content_block_delta, ...)
//!     instead of OpenAI delta objects

use super::super::adapter::{Capability, ProviderAdapter, ProviderStream};
use crate::error::AppError;
use crate::error::AppResult;
use crate::schema::canonical::*;
use async_stream::try_stream;
use futures::future::BoxFuture;
use futures::FutureExt;
use futures::StreamExt;
use http::{HeaderName, HeaderValue};
use serde_json::{json, Value};

pub struct AnthropicAdapter {
    id: String,
    base_url: String,
    default_model: String,
    version: String,
}

impl AnthropicAdapter {
    pub fn new(
        id: impl Into<String>,
        base_url: impl Into<String>,
        default_model: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into(),
            default_model: default_model.into(),
            version: "2023-06-01".to_string(),
        }
    }
}

impl ProviderAdapter for AnthropicAdapter {
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
            HeaderName::from_static("x-api-key"),
            HeaderValue::from_str(key).unwrap_or(HeaderValue::from_static("invalid")),
        )
    }

    fn build_body(&self, req: &CanonicalRequest) -> Value {
        let messages: Vec<Value> = req.messages.iter().map(message_to_anthropic).collect();

        let mut body = json!({
            "model": req.selected_model,
            "messages": messages,
            "max_tokens": req.max_tokens.unwrap_or(4096),
            "stream": req.stream,
        });

        if let Some(sys) = &req.system {
            body["system"] = json!(sys);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(p) = req.top_p {
            body["top_p"] = json!(p);
        }
        if let Some(stop) = &req.stop {
            body["stop_sequences"] = json!(stop);
        }
        if let Some(tools) = &req.tools {
            body["tools"] = json!(tools
                .iter()
                .map(|t| json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                }))
                .collect::<Vec<_>>());
        }
        if let Some(tc) = &req.tool_choice {
            body["tool_choice"] = json!(match tc {
                ToolChoice::Auto => json!({"type": "auto"}),
                ToolChoice::None => json!({"type": "none"}),
                ToolChoice::Required => json!({"type": "any"}),
                ToolChoice::Specific { name } => json!({"type": "tool", "name": name}),
            });
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
            let url = format!("{}/v1/messages", self.base_url);
            let (auth_name, auth_val) = self.auth_header(key);
            let body = self.build_body(req);
            let mut b = body.clone();
            if let Value::Object(ref mut m) = b {
                m.insert("stream".to_string(), Value::Bool(false));
            }

            let resp = client
                .post(&url)
                .header(auth_name, auth_val)
                .header("anthropic-version", &self.version)
                .header("content-type", "application/json")
                .json(&b)
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
            parse_anthropic_response(&v, &self.id)
        })
    }

    /// Anthropic also exposes GET /v1/models but the auth is
    /// x-api-key + anthropic-version, not Bearer.
    fn list_models<'a>(
        &'a self,
        key: &'a str,
        client: &'a reqwest::Client,
    ) -> BoxFuture<'a, AppResult<Vec<String>>> {
        async move {
            let url = format!("{}/v1/models", self.base_url.trim_end_matches('/'));
            let resp = client
                .get(&url)
                .header("x-api-key", key)
                .header("anthropic-version", &self.version)
                .send()
                .await
                .map_err(|e| AppError::Internal(format!("anthropic models list: {e}")))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(AppError::Internal(format!(
                    "anthropic models list {} {}: {}",
                    status.as_u16(),
                    status.canonical_reason().unwrap_or(""),
                    text.chars().take(200).collect::<String>()
                )));
            }
            let v: Value = resp
                .json()
                .await
                .map_err(|e| AppError::Internal(format!("anthropic models parse: {e}")))?;
            // Anthropic shape: { "data": [{"id": "claude-...", ...}, ...] }
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

    fn stream<'a>(
        &'a self,
        req: &'a CanonicalRequest,
        key: &'a str,
        client: &'a reqwest::Client,
    ) -> futures::future::BoxFuture<'a, AppResult<ProviderStream>> {
        Box::pin(async move {
            let url = format!("{}/v1/messages", self.base_url);
            let (auth_name, auth_val) = self.auth_header(key);
            let mut body = self.build_body(req);
            if let Value::Object(ref mut m) = body {
                m.insert("stream".to_string(), Value::Bool(true));
            }

            let resp = client
                .post(&url)
                .header(auth_name, auth_val)
                .header("anthropic-version", &self.version)
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
            let provider_id = self.id.clone();
            let message_id = format!("msg_{}", uuid::Uuid::new_v4());

            let byte_stream = resp.bytes_stream();
            let s = try_stream! {
                let mut buf: Vec<u8> = Vec::new();
                let mut emitted_text = String::new();

                tokio::pin!(byte_stream);
                while let Some(chunk) = byte_stream.next().await {
                    let chunk = chunk?;
                    buf.extend_from_slice(&chunk);
                    while let Some(pos) = find_sse_boundary(&buf) {
                        let raw: Vec<u8> = buf.drain(..pos).collect();
                        let _ = buf.drain(..2); // strip \n\n delimiter
                        let text = String::from_utf8_lossy(&raw);
                        for line in text.lines() {
                            if let Some(data) = line.strip_prefix("data: ") {
                                if data == "[DONE]" {
                                    continue;
                                }
                                if let Ok(v) = serde_json::from_str::<Value>(data) {
                                    if let Some(event) = v.get("type").and_then(|t| t.as_str()) {
                                        match event {
                                            "content_block_delta" => {
                                                if let Some(delta) = v.get("delta") {
                                                    if delta.get("type").and_then(|t| t.as_str()) == Some("text_delta") {
                                                        if let Some(t) = delta.get("text").and_then(|x| x.as_str()) {
                                                            emitted_text.push_str(t);
                                                            yield CanonicalChunk {
                                                                id: message_id.clone(),
                                                                model: model_id.clone(),
                                                                delta: ContentDelta {
                                                                    text: Some(t.to_string()),
                                                                    tool_use: None,
                                                                },
                                                                finish_reason: None,
                                                                usage: None,
                                                            };
                                                        }
                                                    }
                                                }
                                            }
                                            "message_delta" => {
                                                if let Some(usage) = v.get("usage") {
                                                    let out_tok = usage
                                                        .get("output_tokens")
                                                        .and_then(|x| x.as_u64())
                                                        .unwrap_or(0)
                                                        as u32;
                                                    let chunk_out = CanonicalChunk {
                                                        id: message_id.clone(),
                                                        model: model_id.clone(),
                                                        delta: ContentDelta::default(),
                                                        finish_reason: v
                                                            .get("delta")
                                                            .and_then(|d| d.get("stop_reason"))
                                                            .and_then(|s| s.as_str())
                                                            .map(String::from),
                                                        usage: Some(Usage {
                                                            input_tokens: 0,
                                                            output_tokens: out_tok,
                                                            cache_read_tokens: None,
                                                            cache_write_tokens: None,
                                                        }),
                                                    };
                                                    yield chunk_out;
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Final terminal chunk if the upstream didn't send message_delta
                if !emitted_text.is_empty() {
                    yield CanonicalChunk {
                        id: message_id.clone(),
                        model: model_id.clone(),
                        delta: ContentDelta::default(),
                        finish_reason: Some("end_turn".to_string()),
                        usage: None,
                    };
                }
            };

            Ok(s.boxed())
        })
    }
}

fn message_to_anthropic(msg: &CanonicalMessage) -> Value {
    match msg.role {
        Role::User => {
            json!({
                "role": "user",
                "content": blocks_to_anthropic_content(&msg.content),
            })
        }
        Role::Assistant => {
            let mut parts: Vec<Value> = Vec::new();
            for b in &msg.content {
                if let ContentBlock::Text { text } = b {
                    parts.push(json!({"type": "text", "text": text}));
                }
            }
            if let Some(tcs) = &msg.tool_calls {
                for tc in tcs {
                    parts.push(json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": tc.arguments,
                    }));
                }
            }
            json!({"role": "assistant", "content": parts})
        }
        Role::Tool => {
            // Anthropic represents tool results as user-side tool_result blocks.
            let parts: Vec<Value> = msg
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                    } => Some(json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content
                            .iter()
                            .filter_map(|c| match c {
                                ContentBlock::Text { text } => Some(text.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                    })),
                    _ => None,
                })
                .collect();
            json!({"role": "user", "content": parts})
        }
        Role::System => {
            // Should have been extracted already
            json!({"role": "user", "content": []})
        }
    }
}

fn blocks_to_anthropic_content(blocks: &[ContentBlock]) -> Value {
    let parts: Vec<Value> = blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(json!({"type": "text", "text": text})),
            ContentBlock::Image { media_type, data } => {
                let src = match data {
                    ImageData::Url { url } => json!({"type": "url", "url": url}),
                    ImageData::Base64 { data } => {
                        json!({"type": "base64", "media_type": media_type, "data": data})
                    }
                };
                Some(json!({"type": "image", "source": src}))
            }
            _ => None,
        })
        .collect();
    Value::Array(parts)
}

fn parse_anthropic_response(v: &Value, provider_id: &str) -> AppResult<CanonicalResponse> {
    let id = v
        .get("id")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    let model = v
        .get("model")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    let mut content = Vec::new();
    if let Some(arr) = v.get("content").and_then(|x| x.as_array()) {
        for block in arr {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                        content.push(ContentBlock::Text {
                            text: t.to_string(),
                        });
                    }
                }
                Some("tool_use") => {
                    let id = block
                        .get("id")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let name = block
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    content.push(ContentBlock::ToolUse { id, name, input });
                }
                _ => {}
            }
        }
    }
    let finish_reason = v
        .get("stop_reason")
        .and_then(|x| x.as_str())
        .map(String::from);
    let usage = v
        .get("usage")
        .map(|u| Usage {
            input_tokens: u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
            output_tokens: u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
            cache_read_tokens: u
                .get("cache_read_input_tokens")
                .and_then(|x| x.as_u64())
                .map(|x| x as u32),
            cache_write_tokens: u
                .get("cache_creation_input_tokens")
                .and_then(|x| x.as_u64())
                .map(|x| x as u32),
        })
        .unwrap_or_default();

    Ok(CanonicalResponse {
        id,
        model,
        provider: provider_id.to_string(),
        content,
        finish_reason,
        usage,
    })
}

/// Returns the index of `\n\n` (the SSE event boundary) in `buf`,
/// or `None` if no complete event is present yet.
fn find_sse_boundary(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}
