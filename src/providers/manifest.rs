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
///
/// Defaults sourced from `mnfst/manifest/packages/backend/src/routing/oauth/`
/// on `main` — those defaults match the public client_ids the
/// providers ship in their official CLI tools (gemini-cli, openai
/// codex, kiro-cli, etc.), so the user can connect without
/// registering their own OAuth app.
#[derive(Debug, Clone, Copy)]
pub struct ManifestOAuth {
    /// Authorization endpoint for popup_oauth providers. Empty if
    /// not applicable (refresh_token-paste only, or device_code).
    pub authorize_url: &'static str,
    /// Device-code endpoint for device_code providers. POST
    /// returns `{device_code, user_code, verification_uri, ...}`.
    /// Empty if not applicable.
    pub device_code_url: &'static str,
    /// Token URL for device_code providers (returns the access +
    /// refresh tokens after the user authorizes the device). The
    /// main `token_url` above is used for refreshing.
    pub token_url: &'static str,
    /// For Anthropic-style flows: the page that shows the
    /// authorization code the user copies back. Empty otherwise.
    pub paste_code_redirect_url: &'static str,
    /// OAuth client_id (public, embedded in the authorize URL).
    pub client_id: &'static str,
    /// OAuth client_secret. Only Google Gemini's CLI client ships
    /// with a non-empty one; everything else is public-client PKCE.
    pub client_secret: &'static str,
    /// Scope string joined with spaces in the authorize URL.
    pub scope: &'static str,
    /// Override redirect_uri. Default is `${oauth_redirect_uri}/admin/oauth/{provider}/callback`
    /// from `token-dealer.toml`. xAI uses a special 127.0.0.1 path.
    pub redirect_uri: &'static str,
    /// Extra authorize-URL params (Google needs
    /// `access_type=offline&prompt=consent` for refresh tokens).
    /// Each pair is `key=value` appended as-is.
    pub extra_authorize_params: &'static [(&'static str, &'static str)],
    /// `true` for popup flows (requires PKCE), `false` for
    /// device_code / refresh_token-paste.
    pub requires_pkce: bool,
    /// Anthropic's `claude setup-token` flow: the user signs in
    /// on the web, the redirect page shows a code like
    /// `1\\xxxx#yyyy`, they paste it back. No popup, no device
    /// code, no refresh — the "code" is actually the access
    /// token + state tuple.
    pub is_anthropic_paste_code: bool,
    /// Device-code response fields are camelCase (MiniMax) instead
    /// of the standard snake_case (GitHub Copilot, Kiro). When
    /// `true`, we read `deviceCode`/`userCode`/`verificationUri`
    /// instead of `device_code`/`user_code`/`verification_uri`.
    #[allow(dead_code)] pub device_response_camelcase: bool,
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
            // Anthropic uses the `claude setup-token` flow: PKCE authorize →
            // console.anthropic.com displays `code#state` → user pastes
            // back. The token URL exchanges code → access+refresh tokens
            // compatible with Claude Code-style routers.
            oauth: Some(ManifestOAuth {
                authorize_url: "https://claude.ai/oauth/authorize",
                token_url: "https://api.anthropic.com/v1/oauth/token",
                device_code_url: "",
                paste_code_redirect_url:
                    "https://console.anthropic.com/oauth/code/callback",
                client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
                client_secret: "",
                scope: "org:create_api_key user:profile user:inference",
                redirect_uri: "",
                extra_authorize_params: &[],
                requires_pkce: true,
                is_anthropic_paste_code: true,
                device_response_camelcase: false,
            }),
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
            // Google's `gemini-cli` Desktop OAuth client (public client_id).
            // The matching public client_secret lives in `gemini-cli` and
            // is loaded from the `GOOGLE_OAUTH_CLIENT_SECRET` env var at
            // startup (or stays empty for API-key-only setups).
            // Requires access_type=offline + prompt=consent or the second
            // sign-in returns no refresh_token.
            oauth: Some(ManifestOAuth {
                authorize_url: "https://accounts.google.com/o/oauth2/v2/auth",
                token_url: "https://oauth2.googleapis.com/token",
                device_code_url: "",
                paste_code_redirect_url: "",
                client_id:
                    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmi99ikee6.apps.googleusercontent.com",
                client_secret: "", // ← set via GOOGLE_OAUTH_CLIENT_SECRET env
                scope: "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile openid",
                redirect_uri: "",
                extra_authorize_params: &[("access_type", "offline"),
                    ("prompt", "consent"), ("include_granted_scopes", "true")],
                requires_pkce: true,
                is_anthropic_paste_code: false,
                device_response_camelcase: false,
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
            // Kiro uses an OIDC device-code flow against the AWS IAM Identity
            // Center endpoint, with a register-client step that issues a
            // dynamic client_id+secret. The grant type for both
            // authorize-poll and token-exchange is
            // `urn:ietf:params:oauth:grant-type:device_code`.
            oauth: Some(ManifestOAuth {
                authorize_url: "",
                token_url:
                    "https://prod.us-east-1.auth.desktop.kiro.dev/oauth/token",
                device_code_url:
                    "https://prod.us-east-1.auth.desktop.kiro.dev/deviceAuthorization",
                paste_code_redirect_url: "",
                // Kiro registers dynamically — we override the
                // device-flow start with a register-client POST.
                client_id: "kiro-cli",
                client_secret: "",
                scope: "codewhisperer:completions codewhisperer:conversations",
                redirect_uri: "",
                extra_authorize_params: &[],
                requires_pkce: false,
                is_anthropic_paste_code: false,
                device_response_camelcase: false,
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
            // OpenAI Codex uses the SAME OAuth client as ChatGPT (the
            // `app_EMoamEEZ73f0CkXaXp7hrann` Desktop client, public).
            // After auth, the proxy routes to /backend-api/codex with the
            // same refresh_token — handled by the openai-codex-session
            // unwrap. This config covers the authorize + token exchange
            // only; the model dispatch lives in providers/adapters/openai.rs.
            oauth: Some(ManifestOAuth {
                authorize_url: "https://auth.openai.com/oauth/authorize",
                token_url: "https://auth.openai.com/oauth/token",
                device_code_url: "",
                paste_code_redirect_url: "",
                client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
                client_secret: "",
                scope: "openid profile email offline_access",
                redirect_uri: "",
                extra_authorize_params: &[],
                requires_pkce: true,
                is_anthropic_paste_code: false,
                device_response_camelcase: false,
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
            // Same client as Responses — the public Codex/ChatGPT Desktop
            // client_id. The refresh_token exchange yields the access_token
            // ChatGPT Plus uses; Manifest routes to the private
            // /backend-api/codex endpoint when this auth mode is detected.
            oauth: Some(ManifestOAuth {
                authorize_url: "https://auth.openai.com/oauth/authorize",
                token_url: "https://auth.openai.com/oauth/token",
                device_code_url: "",
                paste_code_redirect_url: "",
                client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
                client_secret: "",
                scope: "openid profile email offline_access",
                redirect_uri: "",
                extra_authorize_params: &[],
                requires_pkce: true,
                is_anthropic_paste_code: false,
                device_response_camelcase: false,
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
            // xAI uses a 127.0.0.1:1455/callback redirect (not the standard
            // /auth/callback path) and requires a `nonce` in the authorize
            // URL. The Desktop OAuth client is published in the grok-cli
            // tool — token-dealer exposes it as a public default; users
            // can override via env or admin form.
            oauth: Some(ManifestOAuth {
                authorize_url: "https://auth.x.ai/oauth2/authorize",
                token_url: "https://auth.x.ai/oauth2/token",
                device_code_url: "",
                paste_code_redirect_url: "",
                client_id: "b1a00492-073a-47ea-816f-4c329264a828",
                client_secret: "",
                scope: "openid profile email offline_access grok-cli:access api:access",
                redirect_uri: "http://127.0.0.1:1455/callback",
                extra_authorize_params: &[],
                requires_pkce: true,
                is_anthropic_paste_code: false,
                device_response_camelcase: false,
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
            // Minimax uses a custom user-code grant
            // (urn:ietf:params:oauth:grant-type:user_code) — the device
            // /oauth/code endpoint returns `deviceCode`, `userCode`,
            // `verificationUriComplete` (camelCase!) and a
            // `verificationUriComplete` that redirects to
            // platform.minimax.io/oauth-authorize. Response fields are
            // camelCase unlike the standard snaked GitHub/Kiro device
            // flows, flagged by `device_response_camelcase`.
            oauth: Some(ManifestOAuth {
                authorize_url: "",
                token_url: "https://api.minimax.io/oauth/token",
                device_code_url: "https://api.minimax.io/oauth/code",
                paste_code_redirect_url: "",
                client_id: "78257093-7e40-4613-99e0-527b14b39113",
                client_secret: "",
                scope: "group_id profile model.completion",
                redirect_uri: "",
                extra_authorize_params: &[],
                requires_pkce: false,
                is_anthropic_paste_code: false,
                device_response_camelcase: true,
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
            // GitHub's `Iv1.b507a08c87ecfe98` is the public VS Code Copilot
            // OAuth client. Device-code flow: user visits
            // https://github.com/login/device, types the user_code, GitHub
            // exchanges it for an access_token. Token is then exchanged
            // for a short-lived Copilot session token by the adapter
            // (api.github.com/copilot_internal/v2/token).
            oauth: Some(ManifestOAuth {
                authorize_url: "https://github.com/login/oauth/authorize",
                token_url: "https://github.com/login/oauth/access_token",
                device_code_url: "https://github.com/login/device/code",
                paste_code_redirect_url: "",
                client_id: "Iv1.b507a08c87ecfe98",
                client_secret: "",
                scope: "read:user",
                redirect_uri: "",
                extra_authorize_params: &[],
                requires_pkce: false,
                is_anthropic_paste_code: false,
                device_response_camelcase: false,
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
