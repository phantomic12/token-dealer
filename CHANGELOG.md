# Changelog

All notable changes to token-dealer are documented here. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project aims to follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- (see `main` branch commits since the v0.2.0 tag)

## [0.2.0] — 2026-06 (in progress)

The v0.2.0 series is the "production hardening" release. Generated
from a 27-turn `/grill-me` interview; full plan in
`.ai/plans/v0.2.0-hardening.md`. Scope was 7 floor items across
3 phases (P1 Foundation → P2 Safety → P3 Features + Docs) over
~10–12 working days.

### Added

- **Hand-rolled config validator** (`src/config/validate.rs`).
  Three-tier strictness: hard error / warn-once / unknown-field.
  Checks: socket-addr bind, log level, URL shapes, provider-id
  uniqueness, tier provider references, `enc:` prefix shape,
  retry ranges, budget ranges, unknown-field walk against a
  known-set schema. 20 unit tests.
- **`token-dealer check [--config PATH]`** subcommand. Exit codes
  0/1/2 (clean / invalid / warnings-only). Wired into
  `ConfigService::load` and `reload` — both refuse to proceed
  when the validator reports errors.
- **AES-256-GCM encryption at rest with HKDF subkeys**
  (`src/auth/keystore.rs`). Per-purpose subkeys (TOML auth keys,
  SQLite provider credentials) so rotating one domain doesn't
  require re-encrypting the others. On-disk format
  `enc:<base64(nonce || ct || tag)>`. `MasterKey::from_env_strict()`
  refuses to start when the env var is missing and `[auth]`
  is enabled. 10 unit tests.
- **First-run admin bootstrap** (`main.rs::bootstrap_admin_if_needed`).
  When the user table is empty and `[auth].enabled = true`,
  generate a 24-char base64url password, create the first admin
  (admin@local), and print the password ONCE with a clear
  "save this" banner pointing at the rotate endpoint.
  Idempotent.
- **`POST /admin/auth/rotate-password`** (`auth_endpoints.rs`).
  Verifies the current password (Argon2 constant-time),
  updates the hash, prunes other sessions. Returns 401 on
  wrong current, 400 on empty new, 200 on success.
- **In-memory token-bucket rate limiter** (`src/ratelimit.rs`).
  Per-key + global, defaults 60/120 and 600/1200. Applies to
  `/v1/chat/completions`, `/v1/messages`, `/v1/responses` only.
  429 with `Retry-After: <seconds>` and OpenAI-shape error
  envelope on rejection. Counts at request start (failed auth
  doesn't get a free retry). 7 unit tests.
- **Pass-through handlers** (`src/server/passthrough.rs`):
  `POST /v1/messages` (Anthropic Messages) and
  `POST /v1/responses` (OpenAI Responses). No transpilation —
  the inbound path determines the expected wire format, and a
  tier primary whose adapter type doesn't match is rejected
  with a 400 `wire_format_mismatch` envelope. SSE streams
  forward unchanged on matching shapes. 3 unit tests.
- **First-run auto-config**. `ConfigService::load` writes a
  minimal `token-dealer.toml` when the file is missing (the
  admin password lives in the user_store, not the file).
- **No-providers 503**. `/v1/chat/completions` returns
  `503 no_providers_configured` with a pointer to
  `/ui/providers` when no providers are configured. `/health`
  and `/ui/` still work so the WebUI can walk the user through
  adding the first one.
- **Auto-migration from v0.1.x** (`migrate_v0_1_if_needed`).
  Heuristic: no `[ratelimit]` or any plaintext `[[auth.keys]]`.
  Action: backup to `<path>.v0.1.bak`, add empty `[ratelimit]`
  with defaults, encrypt auth keys in place if
  `ROUTER_MASTER_KEY` is set. Non-interactive. Idempotent.
- **CI hardening** (`.github/workflows/build.yml`):
  - `fmt-clippy` job is now hard-blocking (no more
    `continue-on-error: true`).
  - `cargo audit` via `taiki-e/install-action` (non-blocking,
    surface only).
  - `provenance: true` on `docker/build-push-action` (SLSA-style
    supply chain metadata).
  - `release` job on tag push — builds multi-platform binaries
    (`linux/amd64`, `linux/arm64`, `macos/amd64`, `macos/arm64`,
    `windows/amd64`) and uploads artifacts to the GitHub
    Release.
  - `msrv` job (Rust 1.75) as a separate non-blocking check
    that catches accidental MSRV breaks before they hit stable.
- **Documentation**:
  - `README.md` — production checklist, upgrading from v0.1.x,
    security model, `/v1/*` endpoint matrix.
  - `SECURITY.md` — supported versions, disclosure process
    (security@phantomic.live, 72h ack, coordinated disclosure),
    threat model, cryptography notes, what we don't promise.
  - `CHANGELOG.md` — this file.

### Changed

- `AuthConfig::default()` now sets `warn_fraction = 0.8` correctly
  (the `#[serde(default = ...)]` attribute only applied during
  deserialization, not via `Default::default()`). This was a
  latent bug surfaced by the new validator.
- `Cargo.toml` adds `hkdf = "0.12"` for the new subkey
  derivation path.
- `MasterKey::from_env_or_generate()` now also accepts
  base64-encoded env var values, not just hex / raw bytes.

### Security

- All `/v1/*` endpoints now require Bearer auth when
  `[auth] enabled = true`. The auth middleware checks
  multi-user API keys first, then session cookies, then legacy
  config-defined keys, then the env-var admin password.
- `enc:`-prefixed values in any `key` field are decrypted at
  dispatch time using a per-purpose HKDF subkey. A wrong
  master key yields a clean decrypt failure (GCM tag check),
  not a panic.
- The 3 chat-shaped endpoints are rate-limited at request
  start. Failed-auth requests don't get a free retry slot.
- The admin bootstrap prints the password ONCE. Subsequent
  restarts do not re-print. The only way to recover a lost
  password is to delete the admin user from the SQLite
  database and let the next start regenerate it.

### Known limitations (deferred to v0.3+)

- **`/ui/*` and `/admin/*` paths are NOT yet gated on auth**
  when `[auth] enabled = true`. Anonymous access still works;
  the plan's "all routes require auth" rule is the only
  follow-up from this series.
- The auth-enabled default stays `false` for v0.2.0 to
  preserve existing dev workflows. Flipping the default is a
  one-line change once `/ui/*` is gated.
- Cross-shape transpilation for `/v1/messages` and
  `/v1/responses` (Anthropic↔OpenAI, Responses↔ChatCompletions)
  is explicitly deferred.
- Circuit breaker, models.dev sync, per-agent rules engine,
  metrics endpoint, mid-stream budget enforcement, fuzz
  tests, signed releases, SBOM, web-based `/setup` wizard,
  Proxmox LXC support, and CONTRIBUTING.md are all deferred.

## [0.1.x] — earlier

Pre-hardening series. The v0.1.x release line delivered the core
proxy: OpenAI-compatible routing, multi-provider registry, tier
scorer, specificity detector, OpenRouter pricing sync, model
discovery, multimodal pass-throughs (image / audio / video),
OAuth device-flow loopback login, and the WebUI.
