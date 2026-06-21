//! Canonical internal schema. Everything outside `providers/adapters/*`
//! speaks this format. Per-adapter translation is the only place
//! provider divergence lives.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        media_type: String,
        data: ImageData,
    },
    Audio {
        media_type: String,
        data: AudioData,
    },
    Video {
        media_type: String,
        data: VideoData,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<ContentBlock>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImageData {
    Url { url: String },
    Base64 { data: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AudioData {
    Base64 { data: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VideoData {
    Url { url: String },
    Base64 { data: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalMessage {
    pub role: Role,
    /// Always a list, never a raw string. Inbound normalizer expands
    /// `"content": "hello"` to `[{"type":"text","text":"hello"}]`.
    pub content: Vec<ContentBlock>,
    /// Optional name (OpenAI `name` field on tool messages, etc.)
    pub name: Option<String>,
    /// OpenAI tool-call id for tool/assistant messages
    pub tool_call_id: Option<String>,
    /// Tool calls requested by the assistant
    pub tool_calls: Option<Vec<CanonicalToolCall>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalTool {
    pub name: String,
    pub description: Option<String>,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Specific { name: String },
}

/// Routing tier. Decoupled from `selected_model` so a future scorer
/// can assign tier before model selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Simple,
    Standard,
    Complex,
    Reasoning,
    HighContext,
    Multimodal,
}

impl Tier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Tier::Simple => "simple",
            Tier::Standard => "standard",
            Tier::Complex => "complex",
            Tier::Reasoning => "reasoning",
            Tier::HighContext => "high_context",
            Tier::Multimodal => "multimodal",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "simple" => Tier::Simple,
            "standard" => Tier::Standard,
            "complex" => Tier::Complex,
            "reasoning" => Tier::Reasoning,
            "high_context" | "high-context" => Tier::HighContext,
            "multimodal" | "mm" => Tier::Multimodal,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalRequest {
    pub messages: Vec<CanonicalMessage>,
    pub system: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub stop: Option<Vec<String>>,
    pub stream: bool,
    pub tools: Option<Vec<CanonicalTool>>,
    pub tool_choice: Option<ToolChoice>,

    // ---- Added by the router, not present in the wire request ----
    pub tier: Tier,
    pub selected_model: String,
    pub selected_provider: String,
    pub request_id: Uuid,

    /// Pass-through bucket for unknown provider-specific params.
    /// Each adapter decides whether to forward them.
    #[serde(default)]
    pub extensions: HashMap<String, serde_json::Value>,

    /// Per-request metadata populated by the chat handler. Used for
    /// agent-type detection, debug fields in the request log, and
    /// future per-agent routing rules.
    #[serde(default)]
    pub metadata: CanonicalMetadata,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CanonicalMetadata {
    /// Detected agent type from the User-Agent (claude_code, cursor,
    /// aider, etc.). Set by the chat handler before dispatch.
    pub agent_type: Option<String>,
    /// Client-supplied request ID for end-to-end tracing.
    pub client_request_id: Option<String>,
    /// Detected specificity category (coding, web_browsing, ...) when
    /// the specificity detector fires. `None` means the request was
    /// routed by tier alone (no category activated).
    #[serde(default)]
    pub specificity_category: Option<String>,
    /// Score that triggered the specificity override (debug aid).
    #[serde(default)]
    pub specificity_score: Option<u32>,
    /// Threshold the score had to clear (debug aid).
    #[serde(default)]
    pub specificity_threshold: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalResponse {
    pub id: String,
    pub model: String,
    pub provider: String,
    pub content: Vec<ContentBlock>,
    pub finish_reason: Option<String>,
    pub usage: Usage,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: Option<u32>,
    pub cache_write_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalChunk {
    pub id: String,
    pub model: String,
    pub delta: ContentDelta,
    pub finish_reason: Option<String>,
    /// Provider-reported usage, only present on terminal chunks
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentDelta {
    pub text: Option<String>,
    pub tool_use: Option<CanonicalToolCall>,
}
