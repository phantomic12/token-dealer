//! Per-provider metadata: default base URL, default model, default path.
//! Sourced from `phantomic12/manifest/packages/shared/src/providers.ts` +
//! `.../routing/proxy/provider-endpoints.ts` on `fork/main`.
//!
//! Adding a new provider = one row here + one match arm in
//! `ProviderRegistry::from_configs`. The user can still override every
//! field per-provider in `token-dealer.toml`.

use super::super::config::types::ProviderType;

/// Per-provider OAuth configuration. When set, the `key` field
/// in the provider config is treated as a refresh token — the
/// system exchanges it for an access token via `token_url` and
/// uses the access token in API calls. The user-facing config
/// still looks like a single "key" field; this is internal.
#[derive(Debug, Clone, Copy)]
pub struct ManifestOAuth {
    pub token_url: &'static str,
    pub client_id: &'static str,
    /// Authorization endpoint for popup_oauth providers. Empty if
    /// not applicable (refresh_token-paste only, or device_code).
    pub authorize_url: &'static str,
    /// Token URL for device_code providers (returns the access +
    /// refresh tokens after the user authorizes the device). The
    /// main `token_url` above is used for refreshing.
    pub device_token_url: &'static str,
    /// Device-code endpoint for device_code providers. POST
    /// returns `{device_code, user_code, verification_uri, ...}`.
    /// Empty if not applicable.
    pub device_code_url: &'static str,
}

/// Subscription metadata for token-mode providers. These are not
/// OAuth — the user pastes a token/key with a specific prefix and
/// the system uses it directly. The metadata is used by the UI to
/// show the right placeholder and validate the key format.
#[derive(Debug, Clone, Copy)]
pub struct Subscription {
    pub label: &'static str,
    pub token_prefix: &'static str,
}

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
    /// When set, the `key` field is a refresh token and the
    /// `OAuthManager` will exchange it for an access token.
    pub oauth: Option<ManifestOAuth>,
    /// When set, the provider has a subscription mode where the
    /// user pastes a token with a specific prefix. The system
    /// stores it as a plain API key (no refresh needed).
    pub subscription: Option<Subscription>,
}

/// All known provider types. Used by the wizard UI to render the
/// picker grid.
pub const ALL_TYPES: &[ProviderType] = &[
    ProviderType::Anthropic,
    ProviderType::Google,
    ProviderType::Kiro,
    ProviderType::Responses,
    ProviderType::Generic,
    ProviderType::Openai,
    ProviderType::Openrouter,
    ProviderType::Tokenrouter,
    ProviderType::Groq,
    ProviderType::Deepseek,
    ProviderType::Fireworks,
    ProviderType::Mistral,
    ProviderType::Xai,
    ProviderType::Qwen,
    ProviderType::Moonshot,
    ProviderType::Zai,
    ProviderType::Xiaomi,
    ProviderType::Minimax,
    ProviderType::Byteplus,
    ProviderType::Nvidia,
    ProviderType::OpencodeGo,
    ProviderType::OpencodeZen,
    ProviderType::Kilo,
    ProviderType::Commandcode,
    ProviderType::GithubCopilot,
    ProviderType::Gitlawb,
    ProviderType::Ollama,
    ProviderType::OllamaCloud,
    ProviderType::LlamaCpp,
    ProviderType::LmStudio,
];

pub fn lookup(pt: ProviderType) -> Option<ManifestProvider> {
    Some(match pt {
        ProviderType::Generic => return None,

        // Wire formats
        ProviderType::Anthropic => ManifestProvider {
            base_url: "https://api.anthropic.com",
            default_model: "claude-sonnet-4-5",
            path: "/v1/messages",
            requires_key: true,
            local_only: false,
            oauth: None,
            subscription: Some(Subscription {
                label: "Claude Max / Pro subscription",
                token_prefix: "sk-ant-oat",
            }),
        },
        ProviderType::Google => ManifestProvider {
            base_url: "https://generativelanguage.googleapis.com",
            default_model: "gemini-2.0-flash",
            path: "", // set per-model in the adapter
            requires_key: true,
            local_only: false,
            oauth: Some(ManifestOAuth {
                token_url: "https://oauth2.googleapis.com/token",
                client_id: "681255809395-oo8ft2oprdrnp9e3aqf6av3hmi99ikee6.apps.googleusercontent.com",
                authorize_url: "https://accounts.google.com/o/oauth2/v2/auth",
                device_code_url: "",
                device_token_url: "",
            }),
            subscription: Some(Subscription {
                label: "Sign in with Google (CodeAssist)",
                token_prefix: "",
            }),
        },
        ProviderType::Kiro => ManifestProvider {
            base_url: "https://q.us-east-1.amazonaws.com",
            default_model: "kiro/claude-sonnet-4-5",
            path: "/",
            requires_key: true,
            local_only: false,
            oauth: Some(ManifestOAuth {
                token_url: "https://prod.us-east-1.auth.desktop.kiro.dev/refreshToken",
                client_id: "kiro-cli",
                authorize_url: "",
                device_code_url: "https://prod.us-east-1.auth.desktop.kiro.dev/deviceCode",
                device_token_url: "https://prod.us-east-1.auth.desktop.kiro.dev/deviceToken",
            }),
            subscription: Some(Subscription {
                label: "Kiro subscription",
                token_prefix: "",
            }),
        },
        ProviderType::Responses => ManifestProvider {
            base_url: "https://api.openai.com",
            default_model: "o3",
            path: "/v1/responses",
            requires_key: true,
            local_only: false,
            oauth: Some(ManifestOAuth {
                token_url: "https://auth.openai.com/oauth/token",
                client_id: "app_DoG7JaCkAU8T6mongo4zR1vM",
                authorize_url: "https://auth.openai.com/oauth/authorize",
                device_code_url: "",
                device_token_url: "",
            }),
            subscription: Some(Subscription {
                label: "ChatGPT Plus/Pro/Team",
                token_prefix: "",
            }),
        },
        // OpenAI-compat providers
        ProviderType::Openai => ManifestProvider {
            base_url: "https://api.openai.com",
            default_model: "gpt-4o",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: Some(ManifestOAuth {
                token_url: "https://auth.openai.com/oauth/token",
                client_id: "app_DoG7JaCkAU8T6mongo4zR1vM",
                authorize_url: "https://auth.openai.com/oauth/authorize",
                device_code_url: "",
                device_token_url: "",
            }),
            subscription: Some(Subscription {
                label: "ChatGPT Plus/Pro/Team",
                token_prefix: "",
            }),
        },
        ProviderType::Openrouter => ManifestProvider {
            base_url: "https://openrouter.ai",
            default_model: "anthropic/claude-sonnet-4-5",
            path: "/api/v1/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: None,
            subscription: None,
        },
        ProviderType::Tokenrouter => ManifestProvider {
            base_url: "https://api.tokenrouter.com",
            default_model: "anthropic/claude-sonnet-4-5",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: None,
            subscription: None,
        },
        ProviderType::Groq => ManifestProvider {
            base_url: "https://api.groq.com/openai",
            default_model: "llama-3.3-70b-versatile",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: None,
            subscription: None,
        },
        ProviderType::Deepseek => ManifestProvider {
            base_url: "https://api.deepseek.com",
            default_model: "deepseek-chat",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: None,
            subscription: None,
        },
        ProviderType::Fireworks => ManifestProvider {
            base_url: "https://api.fireworks.ai/inference",
            default_model: "accounts/fireworks/models/llama-v3p3-70b-instruct",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: None,
            subscription: None,
        },
        ProviderType::Mistral => ManifestProvider {
            base_url: "https://api.mistral.ai",
            default_model: "mistral-large-latest",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: None,
            subscription: None,
        },
        ProviderType::Xai => ManifestProvider {
            base_url: "https://api.x.ai",
            default_model: "grok-2-latest",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: Some(ManifestOAuth {
                token_url: "https://auth.x.ai/oauth/token",
                client_id: "xai-cli-public",
                authorize_url: "https://auth.x.ai/oauth/authorize",
                device_code_url: "",
                device_token_url: "",
            }),
            subscription: Some(Subscription {
                label: "Grok subscription",
                token_prefix: "",
            }),
        },
        ProviderType::Qwen => ManifestProvider {
            base_url: "https://dashscope.aliyuncs.com/compatible-mode",
            default_model: "qwen-plus",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: None,
            subscription: Some(Subscription {
                label: "Qwen Token Plan",
                token_prefix: "sk-sp-",
            }),
        },
        ProviderType::Moonshot => ManifestProvider {
            base_url: "https://api.moonshot.ai",
            default_model: "kimi-k2-0711-preview",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: None,
            subscription: Some(Subscription {
                label: "Kimi Coding Plan",
                token_prefix: "",
            }),
        },
        ProviderType::Zai => ManifestProvider {
            base_url: "https://api.z.ai",
            default_model: "glm-4.5",
            path: "/api/paas/v4/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: None,
            subscription: Some(Subscription {
                label: "GLM Coding Plan",
                token_prefix: "",
            }),
        },
        ProviderType::Xiaomi => ManifestProvider {
            base_url: "https://api.xiaomimimo.com",
            default_model: "mimo-v2-flash",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: None,
            subscription: Some(Subscription {
                label: "Xiaomi MiMo Token Plan",
                token_prefix: "tp-",
            }),
        },
        ProviderType::Minimax => ManifestProvider {
            base_url: "https://api.minimax.io",
            default_model: "MiniMax-Text-01",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: Some(ManifestOAuth {
                token_url: "https://api.minimax.chat/oauth/token",
                client_id: "minimax-cli",
                authorize_url: "",
                device_code_url: "https://api.minimax.chat/oauth/device/code",
                device_token_url: "https://api.minimax.chat/oauth/token",
            }),
            subscription: Some(Subscription {
                label: "MiniMax Coding Plan",
                token_prefix: "",
            }),
        },
        ProviderType::Byteplus => ManifestProvider {
            base_url: "https://ark.ap-southeast.bytepluses.com/api/v3",
            default_model: "ep-20240520-vision",
            path: "/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: None,
            subscription: Some(Subscription {
                label: "ModelArk Coding Plan",
                token_prefix: "",
            }),
        },
        ProviderType::Nvidia => ManifestProvider {
            base_url: "https://integrate.api.nvidia.com",
            default_model: "meta/llama-3.1-70b-instruct",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: None,
            subscription: None,
        },
        ProviderType::OpencodeGo => ManifestProvider {
            base_url: "https://opencode.ai/zen/go",
            default_model: "gpt-4o",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: None,
            subscription: Some(Subscription {
                label: "OpenCode Go (beta)",
                token_prefix: "",
            }),
        },
        ProviderType::OpencodeZen => ManifestProvider {
            base_url: "https://opencode.ai/zen",
            default_model: "gpt-4o",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: None,
            subscription: None,
        },
        ProviderType::Kilo => ManifestProvider {
            base_url: "https://api.kilo.ai/api/gateway",
            default_model: "anthropic/claude-sonnet-4-5",
            path: "/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: None,
            subscription: None,
        },
        ProviderType::Commandcode => ManifestProvider {
            base_url: "https://api.commandcode.ai/provider",
            default_model: "gpt-4o",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: None,
            subscription: Some(Subscription {
                label: "Command Code subscription",
                token_prefix: "",
            }),
        },
        ProviderType::GithubCopilot => ManifestProvider {
            base_url: "https://api.githubcopilot.com",
            default_model: "gpt-4o",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: Some(ManifestOAuth {
                token_url: "https://github.com/login/oauth/access_token",
                client_id: "Iv1.b507a08c87ecfe98",
                authorize_url: "https://github.com/login/oauth/authorize",
                device_code_url: "https://github.com/login/device/code",
                device_token_url: "https://github.com/login/oauth/access_token",
            }),
            subscription: Some(Subscription {
                label: "GitHub Copilot subscription",
                token_prefix: "",
            }),
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
            oauth: None,
            subscription: None,
        },
        ProviderType::Ollama => ManifestProvider {
            base_url: "http://localhost:11434",
            default_model: "llama3.1",
            path: "/v1/chat/completions",
            requires_key: false,
            local_only: true,
            oauth: None,
            subscription: None,
        },
        ProviderType::OllamaCloud => ManifestProvider {
            base_url: "https://ollama.com",
            default_model: "llama3.1",
            path: "/v1/chat/completions",
            requires_key: true,
            local_only: false,
            oauth: None,
            subscription: Some(Subscription {
                label: "Ollama Cloud subscription",
                token_prefix: "",
            }),
        },
        ProviderType::LlamaCpp => ManifestProvider {
            base_url: "http://localhost:8080",
            default_model: "default",
            path: "/v1/chat/completions",
            requires_key: false,
            local_only: true,
            oauth: None,
            subscription: None,
        },
        ProviderType::LmStudio => ManifestProvider {
            base_url: "http://localhost:1234",
            default_model: "default",
            path: "/v1/chat/completions",
            requires_key: false,
            local_only: true,
            oauth: None,
            subscription: None,
        },
    })
}

/// Resolve a user-supplied provider ID to its canonical provider type.
/// Used to accept aliases like `opengateway` → `gitlawb` regardless of
/// the `[[providers]]` `id` the user picks.
pub fn resolve_alias(id: &str) -> Option<ProviderType> {
    let lower = id.to_lowercase().replace(['.', '_', ' '], "-");
    let resolved = match lower.as_str() {
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
        "xiaomi" | "mimo" | "xiaomi-mimo" => ProviderType::Xiaomi,
        "minimax" => ProviderType::Minimax,
        "byteplus" | "byteplus-plan" | "modelark" | "modelark-coding-plan" => {
            ProviderType::Byteplus
        }
        "nvidia" | "nvidia-nim" | "nim" => ProviderType::Nvidia,
        "opencode-go" | "opencodego" => ProviderType::OpencodeGo,
        "opencode-zen" | "opencodezen" => ProviderType::OpencodeZen,
        "kilo" | "kilocode" | "kilo-code" => ProviderType::Kilo,
        "commandcode" | "command-code" | "cmd" => ProviderType::Commandcode,
        "copilot" | "github-copilot" | "githubcopilot" => ProviderType::GithubCopilot,
        "gitlawb" | "opengateway" | "open-gateway" | "gl" => ProviderType::Gitlawb,
        "ollama" => ProviderType::Ollama,
        "ollama-cloud" => ProviderType::OllamaCloud,
        "llamacpp" | "llama-cpp" | "llama.cpp" => ProviderType::LlamaCpp,
        "lmstudio" | "lm-studio" => ProviderType::LmStudio,
        "responses" | "openai-responses" | "codex" => ProviderType::Responses,
        "generic" | "custom" | "openai-compat" | "openai-compatible" => {
            return None;
        }
        _ => return None,
    };
    Some(resolved)
}

/// Public alias table for the UI to render. Pairs (alias, canonical).
pub const ALIASES: &[(&str, &str)] = &[
    ("opengateway", "gitlawb"),
    ("open-gateway", "gitlawb"),
    ("gl", "gitlawb"),
    ("kimi", "moonshot"),
    ("moonshotai", "moonshot"),
    ("mimo", "xiaomi"),
    ("xiaomi-mimo", "xiaomi"),
    ("alibaba", "qwen"),
    ("dashscope", "qwen"),
    ("nim", "nvidia"),
    ("nvidia-nim", "nvidia"),
    ("github-copilot", "github-copilot"),
    ("copilot", "github-copilot"),
    ("cmd", "commandcode"),
    ("command-code", "commandcode"),
    ("kilocode", "kilo"),
    ("kilo-code", "kilo"),
    ("grok", "xai"),
    ("x-ai", "xai"),
    ("mistralai", "mistral"),
    ("codex", "responses"),
    ("openai-responses", "responses"),
    ("llama-cpp", "llamacpp"),
    ("llama.cpp", "llamacpp"),
    ("lm-studio", "lmstudio"),
];
