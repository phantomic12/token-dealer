# Security

## Supported versions

| Version  | Status            | Notes                                |
| -------- | ----------------- | ------------------------------------ |
| v0.2.x   | Active            | Security patches backported.         |
| v0.1.x   | EOL since 2026-06 | No further updates. Upgrade to v0.2. |

## Reporting a vulnerability

Email **security@phantomic.live** (PGP key on request). Please include:

- A description of the vulnerability and the attack scenario.
- Reproduction steps, ideally with a minimal config + curl.
- The version of token-dealer you reproduced against.
- Whether you intend to disclose publicly; if so, when.

We commit to:

- **Acknowledge** within **72 hours** of receiving a report.
- **Triage** within 7 days: confirm the report, decide on a fix
  timeline, and reach out to the reporter.
- **Coordinate disclosure**: if a fix is non-trivial, we'll
  agree on a public-disclosure date with the reporter. Default
  is 90 days from confirmation, shorter on request.
- **Credit** the reporter in the release notes if they want it
  (and don't object to it).

## Threat model

token-dealer is a self-hosted LLM routing proxy intended for
deployment by a single operator (the maintainer) plus a small
number of trusted friends. It is **not** designed to be exposed
to untrusted internet traffic with no reverse proxy in front.
The threat model assumes:

- The operator controls the host and the `token-dealer.toml`
  file. A root user on the host can read decrypted keys.
- Clients connect over the loopback or a TLS-terminating reverse
  proxy. The proxy itself does not speak TLS.
- The SQLite database (`token-dealer.db`) is on local disk.
  Backup policy is the operator's responsibility.
- Network egress to upstream providers is allowed (and
  required for the proxy to function).

The proxy **does** defend against:

- Untrusted clients sending Bearer-authenticated requests to
  `/v1/*` (enforced when `[auth] enabled = true`).
- A misconfigured or hostile upstream provider returning 5xx
  (per-tier fallback chain).
- A runaway client exhausting downstream budget
  (per-day + per-request USD caps).
- A large fanout (rate limits per API key + global).

The proxy **does not** defend against:

- A determined attacker with the operator's `ROUTER_MASTER_KEY`
  or a copy of the SQLite database.
- Side-channel attacks on the host (memory disclosure, etc.).
- Provider-side compromise (an attacker who controls OpenAI can
  observe the request).

## Cryptography in v0.2.0

- **Master key**: `ROUTER_MASTER_KEY` env var, 32 bytes raw /
  hex / base64. Required when `[auth] enabled = true`; the
  server refuses to start without it. Generate one with
  `head -c 32 /dev/urandom | base64`.
- **Per-purpose subkeys**: HKDF-SHA256 from the master key,
  one per domain (TOML auth keys, SQLite provider credentials,
  future admin password hash). Rotating one doesn't require
  re-encrypting the others.
- **Symmetric cipher**: AES-256-GCM, fresh random nonce per
  encryption. The on-disk format is
  `enc:<base64(nonce || ciphertext || tag)>`.
- **Password hashing**: Argon2 (via the `argon2` crate) for the
  admin user. The `verify_password` constant-time path is used
  for every login and every rotate-password attempt.

## What we do not promise

- **No FIPS 140-3** mode. AES-GCM is via the `aes-gcm` crate
  which uses RustCrypto, not a FIPS-validated module.
- **No HSM integration**. The master key is process-memory only.
  Operators who need HSM-backed roots should wrap
  `ROUTER_MASTER_KEY` in their own envelope and unwrap before
  starting the process.
- **No side-channel hardening beyond defaults**. We rely on
  RustCrypto's constant-time primitives; no explicit
  masking, jittering, or blinding.
- **No formal security audit**. The codebase has had internal
  review and a 27-question grill-me interview but no
  third-party audit. Treat it as "good engineering practices
  applied carefully" until/unless an audit happens.

## When a CVE is issued

We follow the [GitHub Security Advisories](https://docs.github.com/en/code-security/security-advisories/working-with-global-security-advisories-from-the-github-advisory-database)
format. The maintainer opens a draft advisory privately, links
it to the relevant PR, and publishes a GHSA when the fix ships.
The `cargo audit` integration in the v0.2.0 CI pipeline
(`.github/workflows/build.yml`) surfaces any newly-published
advisories against the dependency tree on every push.
