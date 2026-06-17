//! OpenAI-format inbound → Canonical. The single point where the
//! "content can be a string or a list" divergence is normalized:
//! anything string-typed becomes `[Text]`.

use super::canonical::*;
use crate::error::{AppError, AppResult};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct InboundRequest {
    pub model: String,
    pub messages: Vec<InboundMessage>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub stop: Option<Vec<String>>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub tools: Option<Vec<InboundTool>>,
    #[serde(default)]
    pub tool_choice: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct InboundMessage {
    pub role: String,
    /// Either a string or a list of content blocks. Always normalized
    /// to a list internally.
    pub content: Value,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<InboundToolCall>>,
}

#[derive(Debug, Deserialize)]
pub struct InboundToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: InboundFunction,
}

#[derive(Debug, Deserialize)]
pub struct InboundFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Deserialize)]
pub struct InboundTool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: InboundToolDef,
}

#[derive(Debug, Deserialize)]
pub struct InboundToolDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub parameters: Value,
}

/// Parsed values that the routing layer hasn't touched yet.
pub struct PreRouting {
    pub model_string: String,
    pub request: InboundRequest,
}

impl InboundRequest {
    /// Convert to canonical. `tier` is filled in by the router, not here.
    pub fn into_canonical(
        self,
        tier: Tier,
        selected_model: String,
        selected_provider: String,
        request_id: uuid::Uuid,
    ) -> AppResult<CanonicalRequest> {
        let (system, messages) = extract_system(self.messages);
        let tools = self.tools.map(|t| {
            t.into_iter()
                .map(|tool| CanonicalTool {
                    name: tool.function.name,
                    description: tool.function.description,
                    parameters: tool.function.parameters,
                })
                .collect()
        });
        let tool_choice = self.tool_choice.and_then(|v| parse_tool_choice(&v));

        Ok(CanonicalRequest {
            messages,
            system,
            max_tokens: self.max_tokens,
            temperature: self.temperature,
            top_p: self.top_p,
            stop: self.stop,
            stream: self.stream,
            tools,
            tool_choice,
            tier,
            selected_model,
            selected_provider,
            request_id,
            extensions: HashMap::new(),
            metadata: crate::schema::canonical::CanonicalMetadata::default(),
        })
    }
}

fn parse_tool_choice(v: &Value) -> Option<ToolChoice> {
    match v {
        Value::String(s) => match s.as_str() {
            "auto" => Some(ToolChoice::Auto),
            "none" => Some(ToolChoice::None),
            "required" => Some(ToolChoice::Required),
            _ => None,
        },
        Value::Object(o) => o
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(|n| n.as_str())
            .map(|name| ToolChoice::Specific {
                name: name.to_string(),
            }),
        _ => None,
    }
}

fn extract_system(messages: Vec<InboundMessage>) -> (Option<String>, Vec<CanonicalMessage>) {
    let mut system = None;
    let mut out = Vec::with_capacity(messages.len());
    for msg in messages {
        if msg.role == "system" {
            let text = content_to_text(&msg.content);
            system = Some(match system {
                None => text,
                Some(prev) => format!("{prev}\n{text}"),
            });
        } else {
            out.push(message_to_canonical(msg));
        }
    }
    (system, out)
}

fn message_to_canonical(msg: InboundMessage) -> CanonicalMessage {
    let role = match msg.role.as_str() {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        "system" => Role::System,
        other => {
            tracing::warn!("unknown role {other}, defaulting to user");
            Role::User
        }
    };

    let content = content_to_blocks(&msg.content);
    let tool_calls = msg.tool_calls.map(|calls| {
        calls
            .into_iter()
            .map(|c| CanonicalToolCall {
                id: c.id,
                name: c.function.name,
                arguments: serde_json::from_str(&c.function.arguments)
                    .unwrap_or(serde_json::Value::Null),
            })
            .collect()
    });

    CanonicalMessage {
        role,
        content,
        name: msg.name,
        tool_call_id: msg.tool_call_id,
        tool_calls,
    }
}

fn content_to_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn content_to_blocks(v: &Value) -> Vec<ContentBlock> {
    match v {
        Value::String(s) => vec![ContentBlock::Text { text: s.clone() }],
        Value::Array(arr) => arr.iter().filter_map(block_from_value).collect(),
        _ => vec![ContentBlock::Text {
            text: v.to_string(),
        }],
    }
}

fn block_from_value(v: &Value) -> Option<ContentBlock> {
    let obj = v.as_object()?;
    let kind = obj.get("type")?.as_str()?;
    Some(match kind {
        "text" => ContentBlock::Text {
            text: obj.get("text")?.as_str()?.to_string(),
        },
        "image_url" => {
            let url = obj
                .get("image_url")?
                .as_object()?
                .get("url")?
                .as_str()?
                .to_string();
            let media_type = if url.starts_with("data:") {
                url.split(';').next().unwrap_or("image/*").to_string()
            } else {
                "image/*".to_string()
            };
            ContentBlock::Image {
                media_type,
                data: ImageData::Url { url },
            }
        }
        "image" => {
            let media_type = obj
                .get("source")?
                .as_object()?
                .get("media_type")?
                .as_str()?
                .to_string();
            let data = obj
                .get("source")?
                .as_object()?
                .get("data")?
                .as_str()?
                .to_string();
            ContentBlock::Image {
                media_type,
                data: ImageData::Base64 { data },
            }
        }
        "audio" => {
            let media_type = obj
                .get("source")?
                .as_object()?
                .get("media_type")?
                .as_str()?
                .to_string();
            let data = obj
                .get("source")?
                .as_object()?
                .get("data")?
                .as_str()?
                .to_string();
            ContentBlock::Audio {
                media_type,
                data: AudioData::Base64 { data },
            }
        }
        "tool_use" => ContentBlock::ToolUse {
            id: obj.get("id")?.as_str()?.to_string(),
            name: obj.get("name")?.as_str()?.to_string(),
            input: obj.get("input").cloned().unwrap_or(Value::Null),
        },
        "tool_result" => {
            let tool_use_id = obj.get("tool_use_id")?.as_str()?.to_string();
            let content = obj
                .get("content")
                .map(content_to_blocks)
                .unwrap_or_default();
            ContentBlock::ToolResult {
                tool_use_id,
                content,
            }
        }
        other => {
            tracing::warn!("unknown content block type {other}, dropping");
            return None;
        }
    })
}

/// Helper for handlers — pull model + messages out of a JSON body
/// without yet knowing the tier.
pub fn parse_inbound(body: Value) -> Result<PreRouting, AppError> {
    let req: InboundRequest = serde_json::from_value(body)
        .map_err(|e| AppError::BadRequest(format!("invalid request body: {e}")))?;
    let model_string = req.model.clone();
    Ok(PreRouting {
        model_string,
        request: req,
    })
}
