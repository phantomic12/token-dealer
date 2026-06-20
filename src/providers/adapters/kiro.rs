//! AWS CodeWhisperer "Kiro" streaming API adapter.
//! Wire format is the AWS event-stream binary protocol: each frame is
//! `len(u32) | headers_len(u32) | prelude_crc(u32) | headers | payload | msg_crc(u32)`.
//! Headers are length-prefixed name + type + value; payload is a JSON
//! string. After collecting events we reshape the response into
//! OpenAI-compatible chunks so clients can consume it unchanged.
//!
//! NOTE: This adapter is functional but uses a simplified parser — the
//! manifest version has 100+ lines of frame validation. If you see weird
//! truncation, file an issue and we'll port the strict parser across.

use crate::error::{AppError, AppResult};
use crate::providers::adapter::{Capability, ProviderAdapter, ProviderStream};
use crate::schema::canonical::*;
use async_stream::try_stream;
use bytes::Bytes;
use futures::StreamExt;
use http::{HeaderName, HeaderValue};
use serde_json::{json, Value};

const KIRO_CHAT_TARGET: &str = "AmazonCodeWhispererStreamingService.GenerateAssistantResponse";
const KIRO_ORIGIN: &str = "KIRO_CLI";
const KIRO_AGENT_MODE: &str = "SUPERVISED";

pub struct KiroAdapter {
    id: String,
    base_url: String,
    default_model: String,
}

impl KiroAdapter {
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

impl ProviderAdapter for KiroAdapter {
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

    fn auth_header(&self, _key: &str) -> (HeaderName, HeaderValue) {
        // Kiro uses multiple auth headers; we add them inline in complete/stream.
        (
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("placeholder"),
        )
    }

    fn build_body(&self, req: &CanonicalRequest) -> Value {
        let model = &req.selected_model;
        let model_id = model.strip_prefix("kiro/").unwrap_or(model);

        // Walk messages, build conversation history.
        let mut history: Vec<Value> = Vec::new();
        let mut current_text = String::new();
        for m in &req.messages {
            let text: String = m
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            match m.role {
                Role::System => {
                    if !current_text.is_empty() {
                        current_text.push_str("\n\n");
                    }
                    current_text.push_str(&format!("System instructions:\n{text}"));
                }
                Role::User => {
                    if !current_text.is_empty() {
                        current_text.push_str("\n\n");
                    }
                    current_text.push_str(&format!("User:\n{text}"));
                }
                Role::Assistant => {
                    history.push(json!({"assistantResponseMessage": {"content": text}}));
                }
                Role::Tool => {
                    history.push(json!({
                        "userInputMessage": {
                            "content": format!("Tool result: {text}"),
                            "origin": KIRO_ORIGIN,
                        }
                    }));
                }
            }
        }
        if let Some(sys) = &req.system {
            if !current_text.is_empty() {
                current_text = format!("System instructions:\n{sys}\n\nUser:\n{current_text}");
            } else {
                current_text = format!("System instructions:\n{sys}");
            }
        }
        if current_text.is_empty() {
            current_text = "Hello".to_string();
        }

        json!({
            "conversationState": {
                "conversationId": uuid::Uuid::new_v4().to_string(),
                "history": history,
                "currentMessage": {
                    "userInputMessage": {
                        "content": current_text,
                        "origin": KIRO_ORIGIN,
                        "modelId": model_id,
                    }
                },
                "chatTriggerType": "MANUAL"
            },
            "agentMode": KIRO_AGENT_MODE,
        })
    }

    fn complete<'a>(
        &'a self,
        req: &'a CanonicalRequest,
        key: &'a str,
        client: &'a reqwest::Client,
    ) -> futures::future::BoxFuture<'a, AppResult<CanonicalResponse>> {
        Box::pin(async move {
            let url = &self.base_url;
            let body = self.build_body(req);
            let resp = client
                .post(url)
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/x-amz-json-1.0")
                .header("x-amz-target", KIRO_CHAT_TARGET)
                .header("x-amzn-kiro-agent-mode", KIRO_AGENT_MODE)
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
            // Drain the event stream into a single accumulated response.
            let mut buf = Vec::new();
            let mut content = String::new();
            let mut usage = Usage::default();
            let mut last_chunk = resp.bytes_stream();
            while let Some(chunk) = last_chunk.next().await {
                let chunk = chunk?;
                buf.extend_from_slice(&chunk);
                let events = parse_kiro_events(&mut buf);
                for ev in events {
                    apply_kiro_event(&mut content, &mut usage, &ev);
                }
            }
            Ok(CanonicalResponse {
                id: format!("kiro-{}", uuid::Uuid::new_v4()),
                model: req.selected_model.clone(),
                provider: self.id.clone(),
                content: if content.is_empty() {
                    Vec::new()
                } else {
                    vec![ContentBlock::Text {
                        text: content.clone(),
                    }]
                },
                finish_reason: Some("stop".to_string()),
                usage,
            })
        })
    }

    fn stream<'a>(
        &'a self,
        req: &'a CanonicalRequest,
        key: &'a str,
        client: &'a reqwest::Client,
    ) -> futures::future::BoxFuture<'a, AppResult<ProviderStream>> {
        Box::pin(async move {
            let url = &self.base_url;
            let body = self.build_body(req);
            let resp = client
                .post(url)
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/x-amz-json-1.0")
                .header("x-amz-target", KIRO_CHAT_TARGET)
                .header("x-amzn-kiro-agent-mode", KIRO_AGENT_MODE)
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
            let message_id = format!("kiro-{}", uuid::Uuid::new_v4());
            let mut buf = Vec::new();
            let mut acc = String::new();
            let mut last_usage: Option<Usage> = None;
            let byte_stream = resp.bytes_stream();
            let s = try_stream! {
                tokio::pin!(byte_stream);
                while let Some(chunk) = byte_stream.next().await {
                    let chunk = chunk?;
                    buf.extend_from_slice(&chunk);
                    let events = parse_kiro_events(&mut buf);
                    for ev in events {
                        if let Some(delta) = apply_kiro_event(&mut acc, last_usage.get_or_insert(Usage::default()), &ev) {
                            yield CanonicalChunk {
                                id: message_id.clone(),
                                model: model_id.clone(),
                                delta: ContentDelta {
                                    text: Some(delta),
                                    tool_use: None,
                                },
                                finish_reason: None,
                                usage: None,
                            };
                        }
                    }
                }
                yield CanonicalChunk {
                    id: message_id.clone(),
                    model: model_id.clone(),
                    delta: ContentDelta::default(),
                    finish_reason: Some("stop".to_string()),
                    usage: last_usage.clone(),
                };
            };
            Ok(s.boxed())
        })
    }
}

/// Parses Kiro event-stream frames from `buf`, draining complete frames.
/// Frame layout (per AWS event-stream spec):
///   total_length      u32 BE
///   headers_length    u32 BE
///   prelude_crc       u32 BE  (skipped)
///   headers           [headers_length bytes]
///   payload           [total_length - headers_length - 16 bytes]
///   message_crc       u32 BE  (skipped)
fn parse_kiro_events(buf: &mut Vec<u8>) -> Vec<KiroEvent> {
    let mut out = Vec::new();
    loop {
        if buf.len() < 12 {
            return out;
        }
        let total_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        let headers_len = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
        if total_len < 16 || headers_len + 12 > total_len || buf.len() < total_len {
            return out;
        }
        let payload_start = 12 + headers_len;
        let payload_end = total_len - 4;
        let payload_bytes = &buf[payload_start..payload_end];
        let payload_str = String::from_utf8_lossy(payload_bytes).to_string();
        let payload: Value = serde_json::from_str(&payload_str).unwrap_or(Value::Null);
        let headers = parse_kiro_headers(&buf[12..12 + headers_len]);
        out.push(KiroEvent {
            event_type: headers
                .get(":event-type")
                .and_then(|v| v.as_str())
                .map(String::from),
            message_type: headers
                .get(":message-type")
                .and_then(|v| v.as_str())
                .map(String::from),
            payload,
        });
        buf.drain(..total_len);
    }
}

fn parse_kiro_headers(raw: &[u8]) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    let mut i = 0;
    while i < raw.len() {
        if i + 1 > raw.len() {
            break;
        }
        let name_len = raw[i] as usize;
        i += 1;
        if i + name_len > raw.len() {
            break;
        }
        let name = String::from_utf8_lossy(&raw[i..i + name_len]).to_string();
        i += name_len;
        if i >= raw.len() {
            break;
        }
        let typ = raw[i];
        i += 1;
        let value = match typ {
            0 | 1 => Value::Bool(typ == 0),
            2 => {
                if i >= raw.len() {
                    break;
                }
                let v = raw[i] as i8;
                i += 1;
                Value::from(v)
            }
            7 => {
                if i + 2 > raw.len() {
                    break;
                }
                let len = u16::from_be_bytes([raw[i], raw[i + 1]]) as usize;
                i += 2;
                if i + len > raw.len() {
                    break;
                }
                let s = String::from_utf8_lossy(&raw[i..i + len]).to_string();
                i += len;
                Value::String(s)
            }
            _ => {
                // Unsupported header type — bail out so we don't corrupt
                // the next frame.
                break;
            }
        };
        map.insert(name, value);
    }
    map
}

struct KiroEvent {
    event_type: Option<String>,
    message_type: Option<String>,
    payload: Value,
}

/// Apply a parsed Kiro event to the running accumulator.
/// Returns the text delta if this event emitted content (for streaming).
fn apply_kiro_event(acc: &mut String, usage: &mut Usage, ev: &KiroEvent) -> Option<String> {
    if ev.message_type.as_deref() == Some("exception") {
        tracing::warn!(?ev.payload, "kiro exception event");
        return None;
    }
    let et = ev.event_type.as_deref().unwrap_or("").to_lowercase();
    if et.contains("assistantresponse") {
        if let Some(content) = ev.payload.get("content").and_then(|v| v.as_str()) {
            acc.push_str(content);
            return Some(content.to_string());
        }
    }
    if et.contains("metadata") {
        if let Some(token_usage) = ev.payload.get("tokenUsage") {
            usage.input_tokens = token_usage
                .get("uncachedInputTokens")
                .or_else(|| token_usage.get("inputTokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            usage.output_tokens = token_usage
                .get("outputTokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
        }
    }
    None
}

// Suppress unused warning on Bytes import that some platforms trip on.
#[allow(dead_code)]
fn _bytes_anchor(_: Bytes) {}
