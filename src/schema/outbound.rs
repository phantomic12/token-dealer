//! Canonical → OpenAI-compatible outbound response. Streaming chunks
//! are normalized to `chat.completion.chunk` so any OpenAI SDK can
//! consume them.

use super::canonical::*;
use serde_json::{json, Value};

pub fn response_to_openai(resp: &CanonicalResponse) -> Value {
    json!({
        "id": resp.id,
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": resp.model,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": text_from_blocks(&resp.content),
                "tool_calls": tool_calls_from_blocks(&resp.content),
            },
            "finish_reason": resp.finish_reason,
        }],
        "usage": {
            "prompt_tokens": resp.usage.input_tokens,
            "completion_tokens": resp.usage.output_tokens,
            "total_tokens": resp.usage.input_tokens + resp.usage.output_tokens,
            "cache_read_input_tokens": resp.usage.cache_read_tokens,
            "cache_creation_input_tokens": resp.usage.cache_write_tokens,
        },
    })
}

pub fn chunk_to_openai(chunk: &CanonicalChunk) -> Option<Value> {
    Some(json!({
        "id": chunk.id,
        "object": "chat.completion.chunk",
        "created": chrono::Utc::now().timestamp(),
        "model": chunk.model,
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "content": chunk.delta.text,
                "tool_calls": chunk.delta.tool_use.as_ref().map(|tc| {
                    json!([{
                        "index": 0,
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.name,
                            "arguments": serde_json::to_string(&tc.arguments).unwrap_or_default(),
                        }
                    }])
                }),
            },
            "finish_reason": chunk.finish_reason,
        }],
        "usage": chunk.usage.as_ref().map(|u| json!({
            "prompt_tokens": u.input_tokens,
            "completion_tokens": u.output_tokens,
            "total_tokens": u.input_tokens + u.output_tokens,
        })),
    }))
}

pub fn done_sentinel() -> Value {
    Value::String("[DONE]".to_string())
}

fn text_from_blocks(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn tool_calls_from_blocks(blocks: &[ContentBlock]) -> Option<Value> {
    let calls: Vec<_> = blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => Some(json!({
                "id": id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": serde_json::to_string(input).unwrap_or_default(),
                }
            })),
            _ => None,
        })
        .collect();
    if calls.is_empty() {
        None
    } else {
        Some(Value::Array(calls))
    }
}
