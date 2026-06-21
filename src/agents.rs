//! Agent type detection.
//!
//! Different agents (claude code, cursor, aider, ...) have different
//! default behaviors. We detect them from the User-Agent header and
//! pass that info downstream for:
//!   - Per-agent routing rules (future)
//!   - Usage breakdown by agent in the dashboard
//!
//! Match patterns are conservative — if we don't recognize it, the
//! agent_type is "unknown" and routing falls back to defaults.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentType {
    ClaudeCode,
    Cursor,
    Aider,
    Cody,
    Continue,
    Windsurf,
    Codex,
    Cline,
    Roo,
    /// Generic / unrecognized agent.
    Unknown,
}

impl AgentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentType::ClaudeCode => "claude_code",
            AgentType::Cursor => "cursor",
            AgentType::Aider => "aider",
            AgentType::Cody => "cody",
            AgentType::Continue => "continue",
            AgentType::Windsurf => "windsurf",
            AgentType::Codex => "codex",
            AgentType::Cline => "cline",
            AgentType::Roo => "roo",
            AgentType::Unknown => "unknown",
        }
    }
}

/// Detect the agent from a User-Agent header. Conservative: only
/// match known prefixes / patterns. Falls back to `Unknown`.
pub fn detect_agent(user_agent: Option<&str>) -> AgentType {
    let ua = match user_agent {
        Some(s) => s.to_lowercase(),
        None => return AgentType::Unknown,
    };
    if ua.contains("claude-code") || ua.contains("claude_code") {
        AgentType::ClaudeCode
    } else if ua.contains("cursor") {
        AgentType::Cursor
    } else if ua.contains("aider") {
        AgentType::Aider
    } else if ua.contains("cody") {
        AgentType::Cody
    } else if ua.contains("continue") {
        AgentType::Continue
    } else if ua.contains("windsurf") {
        AgentType::Windsurf
    } else if ua.contains("codex") {
        AgentType::Codex
    } else if ua.contains("cline") {
        AgentType::Cline
    } else if ua.contains("roo") {
        AgentType::Roo
    } else {
        AgentType::Unknown
    }
}
