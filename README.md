# token-dealer

High-performance LLM routing proxy in Rust. Sits between clients and providers, picks the right model for the job, and speaks OpenAI's wire format on the way in.

- **OpenAI-compatible** on the public side — drop-in for any client/SDK
- **Multi-provider** — Anthropic, OpenAI, plus a generic OpenAI-compatible fallback (Together, Groq, Ollama, vLLM, ...)
- **Tier-based routing** — `simple` / `standard` / `complex` / `reasoning` / `high_context` / `multimodal`, per-tier primary + fallback list
- **Two path lengths** — pick by `model: "provider/model"` to bypass tier routing, or by `X-Router-Tier: <tier>` header
- **SSE streaming** with OpenAI-shape chunks + `data: [DONE]` terminator
- **X-Router-\* response headers** — `x-router-provider`, `x-router-model`, `x-router-tier`, `x-router-request-id`

## Quickstart (local)

```bash
# 1. Build
cargo build --release

# 2. Configure
cp token-dealer.toml.example token-dealer.toml
$EDITOR token-dealer.toml
export ANTHROPIC_API_KEY=sk-...

# 3. Run
./target/release/token-dealer
# → listening on 0.0.0.0:8080
```

## Test it

```bash
# Tier-based routing
curl -s http://localhost:8080/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{
    "model": "complex",
    "messages": [{"role": "user", "content": "hi"}]
  }' | jq

# Explicit provider/model
curl -s http://localhost:8080/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{
    "model": "anthropic/claude-sonnet-4-5",
    "messages": [{"role": "user", "content": "hi"}]
  }' | jq

# Stream
curl -N http://localhost:8080/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{
    "model": "anthropic/claude-sonnet-4-5",
    "stream": true,
    "messages": [{"role": "user", "content": "tell me a story"}]
  }'

# List models
curl -s http://localhost:8080/v1/models | jq
```

## Docker

```bash
docker pull ghcr.io/phantomic12/token-dealer:latest
docker run --rm -p 8080:8080 \
  -e ANTHROPIC_API_KEY \
  -e OPENAI_API_KEY \
  -v $PWD/token-dealer.toml:/data/token-dealer.toml:ro \
  ghcr.io/phantomic12/token-dealer:latest
```

Or use the bundled `docker-compose.yml` (mount the config, set env vars, done).

## Routing semantics

| `model` field | Behavior |
|---|---|
| `"provider/model"` (e.g. `anthropic/claude-sonnet-4-5`) | Direct routing, no tier lookup |
| `"tier"` (e.g. `complex`) | Look up the `complex` tier's primary model |
| `"tier/provider/model"` (e.g. `complex/anthropic/claude-opus-4-5`) | Set both tier and route explicitly |
| anything else | Default to configured default_tier, pick that tier's primary |

The `X-Router-Tier` header overrides the tier assignment in all cases.

## Providers

28 provider types, all wired up to manifest-known base URLs + paths. Uncomment the ones you need in `token-dealer.toml`.

| Category | Providers |
|---|---|
| **Native adapters** | `anthropic`, `google` (Gemini generateContent), `kiro` (AWS event stream), `responses` (OpenAI /v1/responses for o1-pro/codex) |
| **OpenAI-compat** | `openai`, `openrouter`, `tokenrouter`, `groq`, `deepseek`, `fireworks`, `mistral`, `xai`, `qwen`, `moonshot`, `zai`, `xiaomi`, `minimax`, `byteplus`, `nvidia`, `opencode-go`, `opencode-zen`, `kilo`, `commandcode`, `github-copilot`, `gitlawb` |
| **Local-only** | `ollama`, `ollama-cloud`, `llamacpp`, `lmstudio` |

Aliases: `opengateway` → `gitlawb`, `kimi` → `moonshot`, `mimo` → `xiaomi`, `alibaba` → `qwen`, `nim` → `nvidia`, `copilot` → `github-copilot`, `cmd` → `commandcode`, `kilocode` → `kilo`, etc. See `src/providers/manifest.rs::resolve_alias` for the full list.

## Architecture

```
src/
├── main.rs                bootstrap, tracing, shutdown signals
├── lib.rs                 AppState, module roots
├── error.rs               AppError → HTTP status mapping (OpenAI error envelope)
├── schema/                canonical types + OpenAI in/out translation
├── providers/             adapter trait + Anthropic + OpenAI + Generic + health + registry
├── routing/               tier scorer + model selector
├── proxy/                 pipeline (route → adapter → response) + SSE
├── config/                TOML types + ConfigService (hot-reload skeleton)
└── server/                axum router, handlers, request-id middleware
```

## Status

This is the MVP. What works:
- OpenAI-compatible `/v1/chat/completions` (non-streaming + SSE)
- OpenAI-compatible `/v1/models`, `/health`
- `/admin/config/reload` (re-reads TOML on the fly)
- Tier-based routing via `model: "tier"` or `X-Router-Tier` header
- Direct routing via `model: "provider/model"`
- Anthropic + OpenAI adapters
- Generic OpenAI-compatible adapter (non-streaming only)
- X-Router-\* response headers + request IDs
- Multi-arch container (linux/amd64, linux/arm64) pushed to GHCR

What's next (phase 2):
- Heuristic scorer (token count, tools, image detection → tier)
- User-defined detection rules engine
- Fallback chains + circuit breaker
- Streaming for the generic adapter
- SQLite-backed request log + cost tracking
- models.dev sync for capability/cost metadata
- Inbound API-key auth
- Admin UI

## License

MIT
