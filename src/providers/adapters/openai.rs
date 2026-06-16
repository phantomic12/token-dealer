//! OpenAI chat-completions adapter. Used for OpenAI itself and as
//! the basis for OpenAI-compatible providers (Together, Groq, etc).

use super::super::adapter::{Capability, ProviderAdapter, ProviderStream};
use crate::error::{AppError, AppResult};
use crate::schema::canonical::*;
use async_stream::try_stream;
use futures::StreamExt;
use http::{HeaderName, HeaderValue};
use serde_json::{json, Value};

pub struct OpenAiAdapter {
    id: String,
    base_url: String,
    path: String,
    default_model: String,
}

impl OpenAiAdapter {
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

impl ProviderAdapter for OpenAiAdapter {
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
            Capability::Context(n) => n <= 128_000,
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
        let mut messages: Vec<Value> = Vec::new();
        if let Some(sys) = &req.system {
            messages.push(json!({"role": "system", "content": sys}));
        }
        for msg in &req.messages {
            messages.push(message_to_openai(msg));
        }

        let mut body = json!({
            "model": req.selected_model,
            "messages": messages,
            "stream": req.stream,
        });
        if let Some(n) = req.max_tokens {
            body["max_tokens"] = json!(n);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(p) = req.top_p {
            body["top_p"] = json!(p);
        }
        if let Some(stop) = &req.stop {
            body["stop"] = json!(stop);
        }
        if let Some(tools) = &req.tools {
            body["tools"] = json!(
                tools
                    .iter()
                    .map(|t| json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    }))
                    .collect::<Vec<_>>()
            );
        }
        if let Some(tc) = &req.tool_choice {
            body["tool_choice"] = match tc {
                ToolChoice::Auto => json!("auto"),
                ToolChoice::None => json!("none"),
                ToolChoice::Required => json!("required"),
                ToolChoice::Specific { name } => {
                    json!({"type": "function", "function": {"name": name}})
                }
            };
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
            let url = format!("{}{}", self.base_url.trim_end_matches('/'), self.path);
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
            parse_openai_response(&v, &self.id)
        })
    }

    fn stream<'a>(
        &'a self,
        req: &'a CanonicalRequest,
        key: &'a str,
        client: &'a reqwest::Client,
    ) -> futures::future::BoxFuture<'a, AppResult<ProviderStream>> {
        Box::pin(async move {
            let url = format!("{}{}", self.base_url.trim_end_matches('/'), self.path);
            let (auth_name, auth_val) = self.auth_header(key);
            let mut body = self.build_body(req);
            if let Value::Object(ref mut m) = body {
                m.insert("stream".to_string(), Value::Bool(true));
            }
            // OpenAI streams need stream_options.include_usage to get token counts.
            if let Value::Object(ref mut m) = body {
                m.insert(
                    "stream_options".to_string(),
                    json!({"include_usage": true}),
                );
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
            let provider_id = self.id.clone();
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
                                if let Some(parsed) = parse_openai_chunk(&v, &model_id, &provider_id) {
                                    yield parsed;
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

fn message_to_openai(msg: &CanonicalMessage) -> Value {
    let role = match msg.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };

    // content can be string (text-only) or array
    let has_non_text = msg
        .content
        .iter()
        .any(|b| !matches!(b, ContentBlock::Text { .. }));

    let content = if has_non_text {
        let parts: Vec<Value> = msg
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(json!({"type": "text", "text": text})),
                ContentBlock::Image { media_type, data } => {
                    let url = match data {
                        ImageData::Url { url } => url.clone(),
                        ImageData::Base64 { data } => {
                            format!("data:{};base64,{}", media_type, data)
                        }
                    };
                    Some(json!({"type": "image_url", "image_url": {"url": url}}))
                }
                _ => None,
            })
            .collect();
        Value::Array(parts)
    } else {
        let text: String = msg
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        Value::String(text)
    };

    let mut out = json!({"role": role, "content": content});
    if let Some(name) = &msg.name {
        out["name"] = json!(name);
    }
    if let Some(id) = &msg.tool_call_id {
        out["tool_call_id"] = json!(id);
    }
    if let Some(tcs) = &msg.tool_calls {
        let calls: Vec<Value> = tcs
            .iter()
            .map(|tc| {
                json!({
                    "id": tc.id,
                    "type": "function",
                    "function": {
                        "name": tc.name,
                        "arguments": serde_json::to_string(&tc.arguments).unwrap_or_default(),
                    }
                })
            })
            .collect();
        out["tool_calls"] = json!(calls);
    }
    out
}

fn parse_openai_response(v: &Value, provider_id: &str) -> AppResult<CanonicalResponse> {
    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let model = v.get("model").and_then(|x| x.as_str()).unwrap_or_default().to_string();

    let mut content = Vec::new();
    if let Some(choice) = v.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first()) {
        if let Some(msg) = choice.get("message") {
            if let Some(s) = msg.get("content").and_then(|x| x.as_str()) {
                content.push(ContentBlock::Text { text: s.to_string() });
            }
            if let Some(tcs) = msg.get("tool_calls").and_then(|x| x.as_array()) {
                for tc in tcs {
                    let id = tc.get("id").and_then(|x| x.as_str()).unwrap_or_default().to_string();
                    let name = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let args_str = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("{}");
                    let arguments = serde_json::from_str(args_str).unwrap_or(Value::Null);
                    content.push(ContentBlock::ToolUse { id, name, input: arguments });
                }
            }
        }
        if let Some(fr) = choice.get("finish_reason").and_then(|x| x.as_str()) {
            return Ok(CanonicalResponse {
                id,
                model,
                provider: provider_id.to_string(),
                content,
                finish_reason: Some(fr.to_string()),
                usage: v.get("usage").map(parse_openai_usage).unwrap_or_default(),
            });
        }
    }

    Ok(CanonicalResponse {
        id,
        model,
        provider: provider_id.to_string(),
        content,
        finish_reason: None,
        usage: v.get("usage").map(parse_openai_usage).unwrap_or_default(),
    })
}

fn parse_openai_usage(u: &Value) -> Usage {
    let prompt_details = u.get("prompt_tokens_details");
    Usage {
        input_tokens: u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        output_tokens: u.get("completion_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        cache_read_tokens: prompt_details
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|x| x.as_u64())
            .map(|x| x as u32),
        cache_write_tokens: None,
    }
}

fn parse_openai_chunk(v: &Value, model_id: &str, provider_id: &str) -> Option<CanonicalChunk> {
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
        .map(|tc| CanonicalToolCall {
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
    let usage = v.get("usage").map(parse_openai_usage);

    Some(CanonicalChunk {
        id,
        model: model_id.to_string(),
        delta: ContentDelta { text, tool_use },
        finish_reason,
        usage,
    })
}

fn find_sse_boundary(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}
