//! Token counting. Uses tiktoken-rs with the cl100k_base encoding
//! (the same encoding GPT-4 / Claude-via-OpenAI-proxy uses). For
//! non-OpenAI models this is a reasonable approximation; tiktoken-rs
//! also supports o200k_base for GPT-4o-class models.
//!
//! Cost calculation is in `super::cost`.

use serde::Serialize;
use std::sync::OnceLock;
use tiktoken_rs::{cl100k_base, o200k_base, CoreBPE};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Encoding {
    Cl100k,
    O200k,
}

impl Encoding {
    pub fn from_model(model: &str) -> Self {
        // GPT-4o and o-series use o200k_base. Everything else falls
        // back to cl100k_base which is a safe approximation for
        // Anthropic, Mistral, Llama, etc.
        let m = model.to_lowercase();
        if m.starts_with("gpt-4o")
            || m.starts_with("o1")
            || m.starts_with("o3")
            || m.starts_with("o4")
            || m.starts_with("chatgpt-4o")
        {
            Encoding::O200k
        } else {
            Encoding::Cl100k
        }
    }
}

static CL100K: OnceLock<CoreBPE> = OnceLock::new();
static O200K: OnceLock<CoreBPE> = OnceLock::new();

fn get_encoder(enc: Encoding) -> &'static CoreBPE {
    match enc {
        Encoding::Cl100k => CL100K.get_or_init(|| cl100k_base().expect("cl100k_base")),
        Encoding::O200k => O200K.get_or_init(|| o200k_base().expect("o200k_base")),
    }
}

/// Count tokens for a single string under the encoding that
/// matches the given model.
pub fn count(model: &str, text: &str) -> u32 {
    let enc = Encoding::from_model(model);
    let bpe = get_encoder(enc);
    bpe.encode_ordinary(text).len() as u32
}

/// Count tokens for a chat-style message list. System prompt +
/// per-message content. Uses the o200k_base / cl100k_base rules
/// for chat-format: 4 tokens of overhead per message, plus the
/// role name.
pub fn count_messages(model: &str, messages: &[(String, String)]) -> u32 {
    let enc = Encoding::from_model(model);
    let bpe = get_encoder(enc);
    // 3 tokens of priming per call
    let mut total: u32 = 3;
    for (role, content) in messages {
        // 4 tokens overhead per message
        total += 4;
        total += count_with(bpe, role);
        total += count_with(bpe, content);
    }
    total
}

fn count_with(bpe: &CoreBPE, s: &str) -> u32 {
    bpe.encode_ordinary(s).len() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cl100k_counts_short_string() {
        // "hello world" → 2 tokens in cl100k_base
        assert_eq!(count("gpt-4", "hello world"), 2);
    }

    #[test]
    fn o200k_for_gpt4o() {
        assert_eq!(Encoding::from_model("gpt-4o"), Encoding::O200k);
        assert_eq!(Encoding::from_model("o1-mini"), Encoding::O200k);
        assert_eq!(Encoding::from_model("claude-3-5-sonnet"), Encoding::Cl100k);
    }
}
