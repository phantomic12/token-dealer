# token-dealer

High-performance LLM routing proxy in Rust. Sits between clients and providers, picks the right model for the job, and speaks OpenAI's wire format on the way in.

- **OpenAI-compatible** on the public side — drop-in for any client/SDK
- **Multi-provider** — Anthropic, OpenAI, plus a generic OpenAI-compatible fallback (Together, Groq, Ollama, vLLM, ...)
- **Tier-based routing** — `simple` / `standard` / `complex` / `reasoning` / `high_context` / `multimodal`, per-tier primary + fallback list
- **Specificity routing** — 9 task categories (coding, web_browsing, data_analysis, image_gen, video_gen, social_media, email_mgmt, calendar_mgmt, trading) detected via keywords + tool-name prefixes. Override with `X-Router-Specificity: <category>`.
- **Two path lengths** — pick by `model: "provider/model"` to bypass tier routing, or by `X-Router-Tier: <tier>` header
- **SSE streaming** with OpenAI-shape chunks + `data: [DONE]` terminator
- **X-Router-\* response headers** — `x-router-provider`, `x-router-model`, `x-router-tier`, `x-router-specificity`, `x-router-request-id`
- **OpenRouter pricing sync** — daily background task ingests the 300+ model price catalog
- **Model discovery** — fetches `/v1/models` from each provider on startup, populates `/v1/models` + tier auto-assignment
- **Cost budgets** — per-day + per-request USD caps with soft-warning + 429 hard-stop
- **SSE event stream** — `/api/v1/events` emits `request.completed` + `budget.warning` + `pricing.synced` events for live UIs

## Auth

Set `[auth] enabled = true` and add one or more `[[auth.keys]]` to require credentials.

```
GET  /v1/models             Authorization: Bearer <key>
POST /v1/chat/completions   Authorization: Bearer <key>
GET  /ui/, /ui/providers    Basic Auth (browser prompts for password, leave username blank)
POST /admin/*               Basic Auth
GET  /health                public (always)
```

Same key table for both Bearer and Basic. Set `key = "${ROUTER_API_KEY}"` to load from the environment. Comparison is constant-time.

## What's left

Medium value: streaming for the generic adapter, circuit breaker probe, models.dev sync, user rules engine editor in the UI, cost-calculation refinements, image/audio/video endpoints. Nice to have: per-modality routing, inbound per-tier key overrides, request budget enforcement mid-stream.

## WebUI

Server-rendered HTML + HTMX. No build step, no Node toolchain — the binary serves it directly. Open `http://localhost:8080/ui/` in a browser.

Three pages:
- **Dashboard** — bind address, config path, provider count, quick-start curl examples
- **Providers** — list + add form (28 manifest types in the dropdown). Add/remove writes to the in-memory registry + persists to `token-dealer.toml`. Each row shows a masked key indicator.
- **Tiers** — per-tier primary editable inline; the form posts to `POST /admin/tiers/:tier` and HTMX swaps the row.

Admin JSON endpoints (for scripts / curl):
```
POST   /admin/providers              # body = ProviderConfig JSON
DELETE /admin/providers/:id
POST   /admin/tiers/:tier            # body = { primary, fallbacks, ... }
POST   /admin/config/save            # force a TOML flush
POST   /admin/config/reload          # re-read TOML from disk
```

**Security note:** the UI is unauthenticated. For local-only use, bind to `127.0.0.1:8080`. For LAN/internet exposure, set `[auth] enabled = true` (inbound API-key enforcement, phase 2) and put it behind a reverse proxy with your own auth.

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
- Specificity routing via `X-Router-Specificity` header or auto-detection
- 28 provider adapters (Anthropic, OpenAI, Google, Kiro, Responses, Generic, ...)
- X-Router-\* response headers + request IDs
- SQLite request log + cost tracking + per-user/per-day spend
- Multi-user auth (argon2) + API key issuance + per-key spend tracking
- OAuth popup flow (Anthropic, ChatGPT Codex) + device_code flow (MiniMax M2)
- OpenRouter pricing sync (daily) + model discovery (startup)
- Cost budgets (per-day, per-request) with soft-warning + 429 hard-stop
- HTMX WebUI (Dashboard, Providers, Tiers, Rules, Logs, Pricing, Users, Account)
- SSE event stream for live UI updates
- Multi-arch container (linux/amd64, linux/arm64) pushed to GHCR

What's next (phase 2):
- Heuristic scorer refinements (tool-aware tier floors)
- Per-agent routing rules (multi-tenant)
- Wingman-style dev drawer
- /v1/messages (Anthropic) + /v1/responses (OpenAI Responses API) pass-throughs

## License

MIT


## v0.2.0 — Production hardening

Release series: P1 (foundation) → P2 (safety) → P3 (features + docs) → rc1 → final.
See `.ai/plans/v0.2.0-hardening.md` for the full plan and 27-turn grill-me interview.

### Production checklist

Before exposing the proxy to the public internet:

1. **Generate a master key**: `head -c 32 /dev/urandom | base64`. Set it as
   `ROUTER_MASTER_KEY` (or point `ROUTER_MASTER_KEY_FILE` at a file with the
   bytes). Without this, `[auth] enabled = true` refuses to start.
2. **Enable auth**: `[auth] enabled = true` in `token-dealer.toml`.
3. **Add real API keys**: either as `[[auth.keys]].key = "sk-..."` (plaintext)
   or `key = "enc:<base64>"` after running through the encrypted path.
4. **Set rate limits per person**: `[ratelimit].per_key.refill_per_minute` (default 60,
   burst 120) and `global.refill_per_minute` (default 600, burst 1200).
5. **Set cost budgets**: `[budgets].daily_cost_usd` and `per_request_cost_usd`
   so a runaway client can't drain a budget.
6. **Put behind a reverse proxy** (nginx, caddy, traefik) for TLS
   termination. token-dealer speaks plain HTTP on the wire by design.
7. **Pin a version**: use a specific image tag (`v0.2.0` not `latest`).
8. **Set log retention**: `[log_retention_days]` defaults to forever; pick
   something sane for your storage budget.
9. **Disable unused features**: `[discovery].enabled = false` if you don't
   want the background model-list sync. `[pricing_sync].enabled = false`
   if you don't want the OpenRouter price catalog.
10. **Read `SECURITY.md`**: support window, disclosure process, what the
    proxy does and does not promise.

### Upgrading from v0.1.x

Drop the new binary in. On first start, token-dealer detects the
v0.1.x config shape (no `[ratelimit]` section, plaintext `[[auth.keys]]`
values) and rewrites it in place:

- Original file → `token-dealer.toml.v0.1.bak` (only written if not
  already present — first migration wins).
- Adds an empty `[ratelimit]` section with the v0.2.0 defaults.
- If `ROUTER_MASTER_KEY` is set, encrypts `[[auth.keys]].key` values
  in place (writes `enc:<...>` back to disk). If not, leaves them
  plaintext and logs a loud warning pointing at the env var.

Migration is non-interactive — `docker compose up -d` in a non-TTY
shell works. If the rewrite fails for any reason, the server logs
the error and continues with the on-disk config as-is. Use
`token-dealer check` for a dry-run validation.

### Security model

Promised (when `[auth] enabled = true`):
- Bearer auth on `/v1/*` (and Basic on `/ui/*` + `/admin/*`).
- Encrypted API keys at rest when `ROUTER_MASTER_KEY` is set
  (AES-256-GCM, per-purpose HKDF subkeys).
- Per-API-key + global rate limits, OpenAI-shape 429 with
  `Retry-After`.
- Per-day + per-request USD cost caps, OpenAI-shape 429.
- SQLite audit log of every request (input tokens, output tokens,
  cost, latency, provider, request id, user).
- Argon2 password hashing for the admin user; rotate via
  `POST /admin/auth/rotate-password`.
- API keys shown once on create, only the sha256 prefix stored.

Not promised (deferred to v0.3+ or out of scope):
- Zero-downtime upgrades. Restart briefly drops in-flight requests.
- Horizontal scale / multi-region HA. Single instance per host.
- Nation-state defense. No rate-limit evasion counter-measures
  beyond the per-key bucket.
- GDPR data-export / data-delete. The audit log retains user
  identifiers; treat the database as in-scope for any compliance
  effort.
- Circuit breaker (deferred to v0.3).
- Generic-adapter SSE (deferred to v0.3).
- Cross-shape transpilation for `/v1/messages` + `/v1/responses`
  (deferred to v0.3).

### `/v1/*` endpoints

| Path                    | Wire format in     | Auth       | Rate-limited |
| ----------------------- | ------------------ | ---------- | ------------ |
| `/v1/chat/completions`  | OpenAI Chat        | Bearer     | yes          |
| `/v1/messages`          | Anthropic Messages | Bearer     | yes          |
| `/v1/responses`         | OpenAI Responses   | Bearer     | yes          |
| `/v1/models`            | OpenAI             | Bearer     | no           |
| `/v1/images/generations`| OpenAI Images      | Bearer     | no           |
| `/v1/audio/speech`      | OpenAI Audio       | Bearer     | no           |
| `/v1/videos/generations`| OpenAI Videos      | Bearer     | no           |
| `/v1/stats`             | n/a (aggregate)    | public     | no           |
| `/health`, `/healthz`   | n/a (liveness)     | public     | no           |

`/v1/messages` and `/v1/responses` are pass-through only in v0.2.0.
The provider's adapter type must match the inbound wire format
(Anthropic / Kiro for messages; OpenAI / OpenRouter for responses),
otherwise the request returns 400 with a `wire_format_mismatch`
error envelope. Use `/v1/chat/completions` for the universal shape
with full cross-provider transpilation.
