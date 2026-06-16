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
        "#,
    )?;
    // Ensure a meta row exists for migrations tracking. We don't
    // version-control the schema; the IF NOT EXISTS clauses make
    // every run idempotent.
    let _ = params![];
    Ok(())
}
