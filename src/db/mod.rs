//! SQLite database: schema migrations + a single connection guarded
//! by a Mutex. All access goes through `tokio::task::spawn_blocking`
//! because rusqlite is sync — keeps the async runtime unblocked
//! even if the DB stalls.

use crate::config::types::DatabaseConfig;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::{Arc, Mutex};

pub mod queries;

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    /// Open or create a SQLite database at `cfg.path`, run migrations.
    /// Path defaults to `:memory:` if `cfg.path` is empty (used by tests).
    pub fn open(cfg: &DatabaseConfig) -> anyhow::Result<Self> {
        let conn = if cfg.path.is_empty() || cfg.path == ":memory:" {
            Connection::open_in_memory()?
        } else {
            if let Some(parent) = Path::new(&cfg.path).parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).ok();
                }
            }
            Connection::open(&cfg.path)?
        };
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        run_migrations(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Run a closure on the connection inside a blocking task. The
    /// closure returns whatever you want; we wrap in `spawn_blocking`
    /// so the async runtime never stalls on disk I/O.
    pub async fn with<F, R>(&self, f: F) -> anyhow::Result<R>
    where
        F: FnOnce(&Connection) -> anyhow::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let g = conn.lock().expect("db mutex poisoned");
            f(&g)
        })
        .await?
    }
}

fn run_migrations(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS request_log (
            id               TEXT PRIMARY KEY,
            created_at       DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
            tier             TEXT NOT NULL,
            requested_model  TEXT,
            routed_model     TEXT NOT NULL,
            routed_provider  TEXT NOT NULL,
            total_latency_ms INTEGER NOT NULL,
            input_tokens     INTEGER,
            output_tokens    INTEGER,
            cache_read_tokens INTEGER,
            cost_usd         REAL,
            truncated        BOOLEAN NOT NULL DEFAULT 0,
            fallback_count   INTEGER NOT NULL DEFAULT 0,
            finished         BOOLEAN NOT NULL DEFAULT 0,
            finish_reason    TEXT,
            client_ip        TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_request_log_created ON request_log(created_at);
        CREATE INDEX IF NOT EXISTS idx_request_log_tier ON request_log(tier);
        CREATE INDEX IF NOT EXISTS idx_request_log_provider ON request_log(routed_provider);

        CREATE TABLE IF NOT EXISTS attempt_log (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            request_id      TEXT NOT NULL REFERENCES request_log(id),
            attempt_number  INTEGER NOT NULL,
            provider        TEXT NOT NULL,
            model           TEXT NOT NULL,
            outcome         TEXT NOT NULL,
            error_code      TEXT,
            error_message   TEXT,
            latency_ms      INTEGER NOT NULL,
            retry_wait_ms   INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_attempt_log_request ON attempt_log(request_id);

        CREATE TABLE IF NOT EXISTS provider_health (
            provider_id          TEXT PRIMARY KEY,
            status               TEXT NOT NULL DEFAULT 'healthy',
            consecutive_failures INTEGER NOT NULL DEFAULT 0,
            last_failure         DATETIME,
            cooldown_until       DATETIME,
            updated_at           DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS provider_credentials (
            provider_id TEXT PRIMARY KEY,
            ciphertext  BLOB NOT NULL,
            nonce       BLOB NOT NULL,
            created_at  DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at  DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS oauth_config (
            provider_id          TEXT PRIMARY KEY,
            token_url            TEXT NOT NULL,
            client_id            TEXT NOT NULL,
            client_secret        TEXT,
            extra_json           TEXT NOT NULL DEFAULT '{}',
            refresh_buffer_secs  INTEGER NOT NULL DEFAULT 300
        );

        -- ── Multi-user + per-user API keys (multi-tenant) ────────────────
        CREATE TABLE IF NOT EXISTS users (
            id           TEXT PRIMARY KEY,
            email        TEXT UNIQUE NOT NULL,
            name         TEXT NOT NULL,
            password_hash TEXT,  -- argon2; nullable for API-key-only users
            role         TEXT NOT NULL DEFAULT 'user',  -- 'admin' | 'user'
            metadata     TEXT NOT NULL DEFAULT '{}',
            created_at   DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
            last_login_at DATETIME
        );

        CREATE TABLE IF NOT EXISTS api_keys (
            id            TEXT PRIMARY KEY,
            user_id       TEXT NOT NULL REFERENCES users(id),
            key_hash      TEXT UNIQUE NOT NULL,  -- sha256 of plaintext key
            key_prefix    TEXT NOT NULL,         -- first 12 chars, for display "tk-abcd1234…"
            name          TEXT NOT NULL DEFAULT 'default',
            last_used_at  DATETIME,
            expires_at    DATETIME,
            created_at    DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
            revoked       BOOLEAN NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_api_keys_user ON api_keys(user_id);
        CREATE INDEX IF NOT EXISTS idx_api_keys_hash ON api_keys(key_hash);

        -- Per-user, per-day token usage. Single row per (user, day).
        -- Used for billing + rate limiting + cost dashboards.
        CREATE TABLE IF NOT EXISTS token_usage (
            user_id        TEXT NOT NULL REFERENCES users(id),
            day            TEXT NOT NULL,  -- YYYY-MM-DD UTC
            input_tokens   INTEGER NOT NULL DEFAULT 0,
            output_tokens  INTEGER NOT NULL DEFAULT 0,
            cost_usd       REAL NOT NULL DEFAULT 0,
            request_count  INTEGER NOT NULL DEFAULT 0,
            updated_at     DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (user_id, day)
        );

        -- Sessions for the WebUI. HttpOnly cookie stores the
        -- session id; the plaintext value is sha256-hashed at rest
        -- (same approach as api_keys).
        CREATE TABLE IF NOT EXISTS sessions (
            id          TEXT PRIMARY KEY,
            user_id     TEXT NOT NULL REFERENCES users(id),
            session_hash TEXT UNIQUE NOT NULL,
            user_agent  TEXT,
            ip          TEXT,
            created_at  DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
            expires_at  DATETIME NOT NULL,
            last_seen_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX IF NOT EXISTS idx_sessions_user ON sessions(user_id);
        CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at);

        -- Per-model pricing. Updated by the pricing-sync background
        -- task. Used for cost computation + tier budget enforcement.
        CREATE TABLE IF NOT EXISTS model_prices (
            model_id            TEXT PRIMARY KEY,
            input_per_1k        REAL NOT NULL DEFAULT 0,
            output_per_1k       REAL NOT NULL DEFAULT 0,
            cached_input_per_1k REAL NOT NULL DEFAULT 0,
            -- Output modality flags (bitfield packed in INTEGER for speed)
            -- bit 0: text, bit 1: image_in, bit 2: image_out,
            -- bit 3: audio_in, bit 4: audio_out, bit 5: video_in, bit 6: video_out
            modality            INTEGER NOT NULL DEFAULT 1,
            context_window      INTEGER NOT NULL DEFAULT 8192,
            updated_at          DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
        );

        -- Per-model default parameter scope. Applied to requests
        -- when the client doesn't specify them. e.g. claude-opus-4
        -- defaults to max_tokens=8192; gpt-4o defaults to 4096.
        CREATE TABLE IF NOT EXISTS model_params (
            model_id     TEXT PRIMARY KEY,
            max_tokens   INTEGER,
            temperature  REAL,
            top_p        REAL,
            extra_json   TEXT NOT NULL DEFAULT '{}',
            updated_at   DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        "#,
    )?;
    // Add the user_id column to request_log if it doesn't exist
    // (idempotent ALTER). Older deployments predate the multi-user
    // schema; this backfills the column.
    let _ = conn.execute(
        "ALTER TABLE request_log ADD COLUMN user_id TEXT",
        rusqlite::params![],
    );
    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_request_log_user ON request_log(user_id)",
        rusqlite::params![],
    );
    // And the user-agent column for agent-type detection.
    let _ = conn.execute(
        "ALTER TABLE request_log ADD COLUMN user_agent TEXT",
        rusqlite::params![],
    );
    let _ = conn.execute(
        "ALTER TABLE request_log ADD COLUMN cost_usd REAL",
        rusqlite::params![],
    );
    // Ensure a meta row exists for migrations tracking. We don't
    // version-control the schema; the IF NOT EXISTS clauses make
    // every run idempotent.
    let _ = params![];

    // Discovery cache (filled at startup by `discover_all`).
    crate::discovery::run_migration(conn)?;

    Ok(())
}
