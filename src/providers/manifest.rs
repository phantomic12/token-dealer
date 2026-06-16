//! Per-provider metadata: default base URL, default model, default path.
//! Sourced from `phantomic12/manifest/packages/shared/src/providers.ts` +
//! `.../routing/proxy/provider-endpoints.ts` on `fork/main`.
//!
//! Adding a new provider = one row here + one match arm in
//! `ProviderRegistry::from_configs`. The user can still override every
//! field per-provider in `token-dealer.toml`.

use super::super::config::types::ProviderType;

#[derive(Debug, Clone, Copy)]
pub struct ManifestProvider {
    pub base_url: &'static str,
    pub default_model: &'static str,
    /// Path appended to `base_url` for chat completions. Most OpenAI-compat
    /// providers use `/v1/chat/completions`; the table below captures the
    /// exceptions (Kilo, BytePlus, Z.ai, GitHub Copilot, etc.).
    pub path: &'static str,
    /// Whether the provider requires an API key.
    pub requires_key: bool,
    /// Whether the provider is local-only (default config has no key).
    pub local_only: bool,
}

pub fn lookup(pt: ProviderType) -> Option<ManifestProvider> {
    Some(match pt {
        // Wire formats
        ProviderType::Anthropic => ManifestProvider {
            base_url: "https://api.anthropic.com",
            default_model: "claude-sonnet-4-5",
            path: "/v1/messages",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Google => ManifestProvider {
            base_url: "https://generativelanguage.googleapis.com",
            default_model: "gemini-2.0-flash",
            path: "", // set per-model in the adapter
            requires_key: true,
            local_only: false,
        },
        ProviderType::Kiro => ManifestProvider {
            base_url: "https://q.us-east-1.amazonaws.com",
            default_model: "kiro/claude-sonnet-4-5",
            path: "/",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Responses => ManifestProvider {
            base_url: "https://api.openai.com",
            default_model: "o3",
            path: "/v1/responses",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Generic => return None,

        // OpenAI-compat providers
        ProviderType::Openai => ManifestProvider {
            base_url: "https://api.openai.com",
            default_model: "gpt-4o",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Openrouter => ManifestProvider {
            base_url: "https://openrouter.ai",
            default_model: "anthropic/claude-sonnet-4-5",
            path: "/api/v1/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Tokenrouter => ManifestProvider {
            base_url: "https://api.tokenrouter.com",
            default_model: "anthropic/claude-sonnet-4-5",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Groq => ManifestProvider {
            base_url: "https://api.groq.com/openai",
            default_model: "llama-3.3-70b-versatile",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Deepseek => ManifestProvider {
            base_url: "https://api.deepseek.com",
            default_model: "deepseek-chat",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Fireworks => ManifestProvider {
            base_url: "https://api.fireworks.ai/inference",
            default_model: "accounts/fireworks/models/llama-v3p3-70b-instruct",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Mistral => ManifestProvider {
            base_url: "https://api.mistral.ai",
            default_model: "mistral-large-latest",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Xai => ManifestProvider {
            base_url: "https://api.x.ai",
            default_model: "grok-2-latest",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Qwen => ManifestProvider {
            base_url: "https://dashscope.aliyuncs.com/compatible-mode",
            default_model: "qwen-plus",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Moonshot => ManifestProvider {
            base_url: "https://api.moonshot.ai",
            default_model: "kimi-k2-0711-preview",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Zai => ManifestProvider {
            base_url: "https://api.z.ai",
            default_model: "glm-4.5",
            path: "/api/paas/v4/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Xiaomi => ManifestProvider {
            base_url: "https://api.xiaomimimo.com",
            default_model: "mimo-v2-flash",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Minimax => ManifestProvider {
            base_url: "https://api.minimax.io",
            default_model: "MiniMax-Text-01",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Byteplus => ManifestProvider {
            base_url: "https://ark.ap-southeast.bytepluses.com/api/v3",
            default_model: "ep-20240520-vision",
            path: "/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Nvidia => ManifestProvider {
            base_url: "https://integrate.api.nvidia.com",
            default_model: "meta/llama-3.1-70b-instruct",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::OpencodeGo => ManifestProvider {
            base_url: "https://opencode.ai/zen/go",
            default_model: "gpt-4o",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::OpencodeZen => ManifestProvider {
            base_url: "https://opencode.ai/zen",
            default_model: "gpt-4o",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Kilo => ManifestProvider {
            base_url: "https://api.kilo.ai/api/gateway",
            default_model: "anthropic/claude-sonnet-4-5",
            path: "/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Commandcode => ManifestProvider {
            base_url: "https://api.commandcode.ai/provider",
            default_model: "gpt-4o",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::GithubCopilot => ManifestProvider {
            base_url: "https://api.githubcopilot.com",
            default_model: "gpt-4o",
            path: "/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Gitlawb => ManifestProvider {
            // OpenGateway is the user-facing name; Gitlawb is the host
            // brand. Same endpoint. Users can also use `id = "opengateway"`
            // — see alias lookup in `ProviderRegistry::from_configs`.
            base_url: "https://opengateway.gitlawb.com",
            default_model: "gpt-4o",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::Ollama => ManifestProvider {
            base_url: "http://localhost:11434",
            default_model: "llama3.1",
            path: "/v1/chat/completions",
            requires_key: false,
            local_only: true,
        },
        ProviderType::OllamaCloud => ManifestProvider {
            base_url: "https://ollama.com",
            default_model: "llama3.1",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
        },
        ProviderType::LlamaCpp => ManifestProvider {
            base_url: "http://localhost:8080",
            default_model: "default",
            path: "/v1/chat/completions",
            requires_key: false,
            local_only: true,
        },
        ProviderType::LmStudio => ManifestProvider {
            base_url: "http://localhost:1234",
            default_model: "default",
            path: "/v1/chat/completions",
            requires_key: false,
            local_only: true,
        },
    })
}

/// Resolve a user-supplied provider ID to its canonical provider type.
/// Used to accept aliases like `opengateway` → `gitlawb` regardless of
/// the `[[providers]]` `id` the user picks.
pub fn resolve_alias(id: &str) -> Option<ProviderType> {
    let lower = id.to_lowercase().replace(['.', '_', ' '], "-");
    Some(match lower.as_str() {
        "anthropic" => ProviderType::Anthropic,
        "openai" | "openai-completions" => ProviderType::Openai,
        "google" | "gemini" => ProviderType::Google,
        "kiro" => ProviderType::Kiro,
        "openrouter" => ProviderType::Openrouter,
        "tokenrouter" => ProviderType::Tokenrouter,
        "groq" => ProviderType::Groq,
        "deepseek" => ProviderType::Deepseek,
        "fireworks" | "fireworks-ai" | "fireworksai" => ProviderType::Fireworks,
        "mistral" | "mistralai" => ProviderType::Mistral,
        "xai" | "x-ai" | "grok" => ProviderType::Xai,
        "qwen" | "alibaba" | "dashscope" => ProviderType::Qwen,
        "moonshot" | "kimi" | "moonshotai" => ProviderType::Moonshot,
        "zai" | "z-ai" | "z.ai" | "zhipuai" => ProviderType::Zai,
        "xiaomi" | "mimo" | "xiaomi-mimo" | "xiaomi-mimo-mimo" => ProviderType::Xiaomi,
        "minimax" => ProviderType::Minimax,
        "byteplus" | "byteplus-plan" | "modelark" | "modelark-coding-plan" => {
            ProviderType::Byteplus
        }
        "nvidia" | "nvidia-nim" | "nvidia-nim-nim" | "nim" => ProviderType::Nvidia,
        "opencode-go" | "opencodego" => ProviderType::OpencodeGo,
        "opencode-zen" | "opencodezen" => ProviderType::OpencodeZen,
        "kilo" | "kilocode" | "kilo-code" => ProviderType::Kilo,
        "commandcode" | "command-code" | "cmd" => ProviderType::Commandcode,
        "copilot" | "github-copilot" | "githubcopilot" => ProviderType::GithubCopilot,
        "gitlawb" | "opengateway" | "open-gateway" | "gl" => ProviderType::Gitlawb,
        "ollama" => ProviderType::Ollama,
        "ollama-cloud" | "ollama-cloud-cloud" => ProviderType::OllamaCloud,
        "llamacpp" | "llama-cpp" | "llama.cpp" => ProviderType::LlamaCpp,
        "lmstudio" | "lm-studio" | "lm-studio-studio" => ProviderType::LmStudio,
        "responses" | "openai-responses" | "codex" => ProviderType::Responses,
        "generic" | "custom" | "openai-compat" | "openai-compatible" => {
            return None;
        }
        _ => return None,
    })
}
