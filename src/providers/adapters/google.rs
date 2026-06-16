//! Google Gemini generateContent / streamGenerateContent API adapter.
//! Differs from OpenAI on:
//!   - endpoint: /v1beta/models/{model}:generateContent (path-built per-model)
//!   - auth: x-goog-api-key header (or `?key=` query param — we use the header)
//!   - body shape: `{contents: [{role, parts: [{text}]}], systemInstruction, generationConfig}`
//!   - response shape: `{candidates: [{content, finishReason}], usageMetadata}`
//!   - streaming: GET-style with `?alt=sse` query param (POST still works)
//!   - tool calls: `{functionCall: {name, args}}` parts (deferred to phase 2)

use crate::providers::adapter::{Capability, ProviderAdapter, ProviderStream};
use crate::error::AppError;
use crate::error::AppResult;
use crate::schema::canonical::*;
use async_stream::try_stream;
use futures::StreamExt;
use http::{HeaderName, HeaderValue};
use serde_json::{json, Value};

pub struct GoogleAdapter {
    id: String,
    base_url: String,
    default_model: String,
}

impl GoogleAdapter {
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

    fn model_path(&self, model: &str, stream: bool) -> String {
        let verb = if stream {
            "streamGenerateContent"
        } else {
            "generateContent"
        };
        let mut url = format!(
            "{}/v1beta/models/{}:{}",
            self.base_url.trim_end_matches('/'),
            model,
            verb
        );
        if stream {
            url.push_str("?alt=sse");
        }
        url
    }
}

impl ProviderAdapter for GoogleAdapter {
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
            Capability::Audio => true,
            Capability::Video => false,
            Capability::Reasoning => true,
            Capability::Context(n) => n <= 1_000_000,
        }
    }

    fn auth_header(&self, key: &str) -> (HeaderName, HeaderValue) {
        (
            HeaderName::from_static("x-goog-api-key"),
            HeaderValue::from_str(key).unwrap_or(HeaderValue::from_static("invalid")),
        )
    }

    fn build_body(&self, req: &CanonicalRequest) -> Value {
        let mut contents: Vec<Value> = Vec::new();
        for m in &req.messages {
            let role = match m.role {
                Role::User | Role::Tool => "user",
                Role::Assistant => "model",
                Role::System => continue,
            };
            let parts: Vec<Value> = m
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(json!({"text": text})),
                    ContentBlock::Image { media_type, data } => match data {
                        ImageData::Base64 { data } => Some(json!({
                            "inline_data": {"mime_type": media_type, "data": data}
                        })),
                        ImageData::Url { url } => {
                            // Gemini's REST API doesn't accept URLs directly
                            // for images. Caller must inline. Skip silently.
                            tracing::warn!("skipping image URL (Gemini requires inline data): {url}");
                            None
                        }
                    },
                    _ => None,
                })
                .collect();
            contents.push(json!({"role": role, "parts": parts}));
        }

        let mut body = json!({
            "contents": contents,
        });

        if let Some(sys) = &req.system {
            body["systemInstruction"] = json!({"parts": [{"text": sys}]});
        }
        let mut gen_config = serde_json::Map::new();
        if let Some(n) = req.max_tokens {
            gen_config.insert("maxOutputTokens".into(), json!(n));
        }
        if let Some(t) = req.temperature {
            gen_config.insert("temperature".into(), json!(t));
        }
        if let Some(p) = req.top_p {
            gen_config.insert("topP".into(), json!(p));
        }
        if let Some(stop) = &req.stop {
            gen_config.insert("stopSequences".into(), json!(stop));
        }
        if !gen_config.is_empty() {
            body["generationConfig"] = Value::Object(gen_config);
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
            let url = self.model_path(&req.selected_model, false);
            let (auth_name, auth_val) = self.auth_header(key);
            let body = self.build_body(req);
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
            parse_google_response(&v, &self.id, &req.selected_model)
        })
    }

    fn stream<'a>(
        &'a self,
        req: &'a CanonicalRequest,
        key: &'a str,
        client: &'a reqwest::Client,
    ) -> futures::future::BoxFuture<'a, AppResult<ProviderStream>> {
        Box::pin(async move {
            let url = self.model_path(&req.selected_model, true);
            let (auth_name, auth_val) = self.auth_header(key);
            let body = self.build_body(req);
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
            let message_id = format!("gemini-{}", uuid::Uuid::new_v4());
            let byte_stream = resp.bytes_stream();
            let s = try_stream! {
                let mut buf: Vec<u8> = Vec::new();
                let mut final_usage: Option<Usage> = None;
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
                            if let Ok(v) = serde_json::from_str::<Value>(data) {
                                if let Some(u) = v.get("usageMetadata") {
                                    final_usage = Some(Usage {
                                        input_tokens: u
                                            .get("promptTokenCount")
                                            .and_then(|x| x.as_u64())
                                            .unwrap_or(0) as u32,
                                        output_tokens: u
                                            .get("candidatesTokenCount")
                                            .and_then(|x| x.as_u64())
                                            .unwrap_or(0) as u32,
                                        cache_read_tokens: None,
                                        cache_write_tokens: None,
                                    });
                                }
                                if let Some(candidates) =
                                    v.get("candidates").and_then(|c| c.as_array())
                                {
                                    if let Some(cand) = candidates.first() {
                                        let finish_reason = cand
                                            .get("finishReason")
                                            .and_then(|x| x.as_str())
                                            .map(|s| match s {
                                                "STOP" => "stop",
                                                "MAX_TOKENS" => "length",
                                                "SAFETY" => "content_filter",
                                                other => other,
                                            })
                                            .map(String::from);
                                        if let Some(parts) = cand
                                            .get("content")
                                            .and_then(|c| c.get("parts"))
                                            .and_then(|p| p.as_array())
                                        {
                                            for part in parts {
                                                if let Some(t) =
                                                    part.get("text").and_then(|x| x.as_str())
                                                {
                                                    if !t.is_empty() {
                                                        yield CanonicalChunk {
                                                            id: message_id.clone(),
                                                            model: model_id.clone(),
                                                            delta: ContentDelta {
                                                                text: Some(t.to_string()),
                                                                tool_use: None,
                                                            },
                                                            finish_reason: finish_reason.clone(),
                                                            usage: None,
                                                        };
                                                    }
                                                }
                                            }
                                        }
                                        if finish_reason.is_some() {
                                            yield CanonicalChunk {
                                                id: message_id.clone(),
                                                model: model_id.clone(),
                                                delta: ContentDelta::default(),
                                                finish_reason: finish_reason.clone(),
                                                usage: final_usage.clone(),
                                            };
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(usage) = final_usage {
                    yield CanonicalChunk {
                        id: message_id.clone(),
                        model: model_id.clone(),
                        delta: ContentDelta::default(),
                        finish_reason: Some("stop".to_string()),
                        usage: Some(usage),
                    };
                }
                let _ = provider_id;
            };
            Ok(s.boxed())
        })
    }
}

fn parse_google_response(
    v: &Value,
    provider_id: &str,
    fallback_model: &str,
) -> AppResult<CanonicalResponse> {
    let id = format!("gemini-{}", uuid::Uuid::new_v4());
    let model = fallback_model.to_string();
    let mut content = Vec::new();
    if let Some(candidates) = v.get("candidates").and_then(|c| c.as_array()) {
        if let Some(cand) = candidates.first() {
            if let Some(parts) = cand
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array())
            {
                for part in parts {
                    if let Some(t) = part.get("text").and_then(|x| x.as_str()) {
                        content.push(ContentBlock::Text { text: t.to_string() });
                    }
                }
            }
        }
    }
    let finish_reason = v
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("finishReason"))
        .and_then(|x| x.as_str())
        .map(|s| match s {
            "STOP" => "stop",
            "MAX_TOKENS" => "length",
            "SAFETY" => "content_filter",
            other => other,
        })
        .map(String::from);
    let usage = v.get("usageMetadata").map(|u| Usage {
        input_tokens: u
            .get("promptTokenCount")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32,
        output_tokens: u
            .get("candidatesTokenCount")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32,
        cache_read_tokens: None,
        cache_write_tokens: None,
    });

    Ok(CanonicalResponse {
        id,
        model,
        provider: provider_id.to_string(),
        content,
        finish_reason,
        usage: usage.unwrap_or_default(),
    })
}

fn find_sse_boundary(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}
