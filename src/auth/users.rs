//! Multi-user authentication.
//!
//! - `User` is the identity. Email is unique. `password_hash` is
//!   argon2 (nullable for API-key-only users). `role` is 'admin' or
//!   'user'.
//! - `ApiKey` is a bearer credential. Plaintext is shown once on
//!   create, only the sha256 hash is stored. Prefix is the first
//!   12 chars, for the UI ("tk-abcd1234…").
//! - `Session` is a WebUI cookie. Plaintext is the cookie value;
//!   the sha256 hash is in `session_hash`. Expires after 30 days
//!   by default; sliding renewal on each request.
//!
//! Resolution priority on a request:
//!   1. `Authorization: Bearer tk-...` → API key
//!   2. `td_session` cookie → session
//!   3. (legacy) `Authorization: Basic ...` → admin via env
//!   4. None → request proceeds as anonymous (allowed on /v1/models,
//!      /v1/stats, /ui/login, /health, /admin/*, etc).

use crate::db::Db;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    User,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::User => "user",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "admin" => Role::Admin,
            _ => Role::User,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub name: String,
    #[serde(skip_serializing)]
    pub password_hash: Option<String>,
    pub role: Role,
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub last_login_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: String,
    pub user_id: String,
    #[serde(skip_serializing)]
    pub key_hash: String,
    /// First 12 chars of the plaintext key, for display
    /// (e.g. "tk-abcd1234"). Never the full key.
    pub key_prefix: String,
    pub name: String,
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub revoked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub user_id: String,
    #[serde(skip_serializing)]
    pub session_hash: String,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

/// What the auth middleware attaches to each request. Handlers
/// read this via `axum::extract::Extension<UserContext>`.
#[derive(Debug, Clone, Serialize)]
pub struct UserContext {
    pub user_id: String,
    pub email: String,
    pub name: String,
    pub role: Role,
    /// How the request was authenticated. Useful for audit logs.
    pub via: &'static str,
    /// API key prefix if the request used one. None for sessions.
    pub key_prefix: Option<String>,
    /// Session id (for renewal / invalidation).
    pub session_id: Option<String>,
}

impl UserContext {
    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }
}

// ── Hashing helpers ─────────────────────────────────────────────────

pub fn hash_password(plain: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon = Argon2::default();
    let hash = argon
        .hash_password(plain.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("argon2 hash failed: {e}"))?
        .to_string();
    Ok(hash)
}

pub fn verify_password(plain: &str, hash: &str) -> bool {
    let parsed = match PasswordHash::new(hash) {
        Ok(p) => p,
        Err(_) => return false,
    };
    Argon2::default()
        .verify_password(plain.as_bytes(), &parsed)
        .is_ok()
}

pub fn sha256_hex(input: &str) -> String {
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    let out = h.finalize();
    hex::encode(out)
}

/// Generate a new API key. Returns the plaintext (shown once to the
/// user) and the sha256 hash (stored in the DB).
pub fn generate_api_key() -> (String, String) {
    let mut buf = [0u8; 32];
    OsRng.fill_bytes(&mut buf);
    let plaintext = format!("tk-{}", hex::encode(buf));
    let hash = sha256_hex(&plaintext);
    (plaintext, hash)
}

/// Generate a new session token. Same shape as API keys.
pub fn generate_session_token() -> (String, String) {
    let mut buf = [0u8; 32];
    OsRng.fill_bytes(&mut buf);
    let plaintext = hex::encode(buf);
    let hash = sha256_hex(&plaintext);
    (plaintext, hash)
}

// ── UserStore: the in-process layer that talks to the DB ──────────

#[derive(Clone)]
pub struct UserStore {
    db: Db,
}

impl UserStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    // Users
    pub async fn create_user(
        &self,
        email: &str,
        name: &str,
        password: Option<&str>,
        role: Role,
    ) -> anyhow::Result<User> {
        let email = email.trim().to_lowercase();
        if email.is_empty() || !email.contains('@') {
            anyhow::bail!("invalid email");
        }
        let id = uuid::Uuid::new_v4().to_string();
        let password_hash = password.map(hash_password).transpose()?;
        let user = User {
            id: id.clone(),
            email,
            name: name.to_string(),
            password_hash,
            role: role.clone(),
            metadata: serde_json::json!({}),
            created_at: Utc::now(),
            last_login_at: None,
        };
        let email_clone = user.email.clone();
        let name_clone = user.name.clone();
        let hash_clone = user.password_hash.clone();
        let role_str = user.role.as_str();
        let meta_str = user.metadata.to_string();
        let id_clone = user.id.clone();
        self.db
            .with(move |c| {
                c.execute(
                    "INSERT INTO users (id, email, name, password_hash, role, metadata) VALUES (?,?,?,?,?,?)",
                    rusqlite::params![id_clone, email_clone, name_clone, hash_clone, role_str, meta_str],
                )?;
                Ok(())
            })
            .await?;
        Ok(user)
    }

    pub async fn get_user(&self, id: &str) -> anyhow::Result<Option<User>> {
        let id = id.to_string();
        self.db
            .with(move |c| {
                let mut stmt = c.prepare(
                    "SELECT id, email, name, password_hash, role, metadata, created_at, last_login_at FROM users WHERE id = ?",
                )?;
                let mut rows = stmt.query([&id])?;
                if let Some(row) = rows.next()? {
                    Ok(Some(row_to_user(row)?))
                } else {
                    Ok(None)
                }
            })
            .await
    }

    pub async fn get_user_by_email(&self, email: &str) -> anyhow::Result<Option<User>> {
        let email = email.to_lowercase();
        self.db
            .with(move |c| {
                let mut stmt = c.prepare(
                    "SELECT id, email, name, password_hash, role, metadata, created_at, last_login_at FROM users WHERE email = ?",
                )?;
                let mut rows = stmt.query([email])?;
                if let Some(row) = rows.next()? {
                    Ok(Some(row_to_user(row)?))
                } else {
                    Ok(None)
                }
            })
            .await
    }

    pub async fn list_users(&self) -> anyhow::Result<Vec<User>> {
        self.db
            .with(|c| {
                let mut stmt = c.prepare(
                    "SELECT id, email, name, password_hash, role, metadata, created_at, last_login_at FROM users ORDER BY created_at ASC",
                )?;
                let rows = stmt
                    .query_map([], row_to_user)?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
    }

    pub async fn delete_user(&self, id: &str) -> anyhow::Result<()> {
        let id = id.to_string();
        self.db
            .with(move |c| {
                c.execute("DELETE FROM api_keys WHERE user_id = ?", [&id])?;
                c.execute("DELETE FROM sessions WHERE user_id = ?", [&id])?;
                c.execute("DELETE FROM token_usage WHERE user_id = ?", [&id])?;
                c.execute("DELETE FROM users WHERE id = ?", [&id])?;
                Ok(())
            })
            .await
    }

    pub async fn touch_last_login(&self, id: &str) -> anyhow::Result<()> {
        let id = id.to_string();
        self.db
            .with(move |c| {
                c.execute(
                    "UPDATE users SET last_login_at = CURRENT_TIMESTAMP WHERE id = ?",
                    [&id],
                )?;
                Ok(())
            })
            .await
    }

    // API keys
    pub async fn create_api_key(
        &self,
        user_id: &str,
        name: &str,
    ) -> anyhow::Result<(ApiKey, String)> {
        let id = uuid::Uuid::new_v4().to_string();
        let (plaintext, hash) = generate_api_key();
        let prefix: String = plaintext.chars().take(12).collect();
        let key = ApiKey {
            id: id.clone(),
            user_id: user_id.to_string(),
            key_hash: hash.clone(),
            key_prefix: prefix.clone(),
            name: name.to_string(),
            last_used_at: None,
            expires_at: None,
            created_at: Utc::now(),
            revoked: false,
        };
        let id_clone = key.id.clone();
        let user_id_clone = key.user_id.clone();
        let hash_clone = key.key_hash.clone();
        let prefix_clone = key.key_prefix.clone();
        let name_clone = key.name.clone();
        self.db
            .with(move |c| {
                c.execute(
                    "INSERT INTO api_keys (id, user_id, key_hash, key_prefix, name) VALUES (?,?,?,?,?)",
                    rusqlite::params![id_clone, user_id_clone, hash_clone, prefix_clone, name_clone],
                )?;
                Ok(())
            })
            .await?;
        Ok((key, plaintext))
    }

    pub async fn get_user_by_api_key(
        &self,
        plaintext: &str,
    ) -> anyhow::Result<Option<(User, ApiKey)>> {
        let hash = sha256_hex(plaintext);
        self.db
            .with(move |c| {
                let mut stmt = c.prepare(
                    "SELECT k.id, k.user_id, k.key_hash, k.key_prefix, k.name, k.last_used_at, k.expires_at, k.created_at, k.revoked,
                            u.id, u.email, u.name, u.password_hash, u.role, u.metadata, u.created_at, u.last_login_at
                     FROM api_keys k JOIN users u ON u.id = k.user_id
                     WHERE k.key_hash = ? AND k.revoked = 0",
                )?;
                let mut rows = stmt.query([hash])?;
                if let Some(row) = rows.next()? {
                    let key = row_to_api_key(row)?;
                    let user = row_to_user_from_key(row)?;
                    // Touch last_used_at in the same query (best-effort)
                    let _ = c.execute(
                        "UPDATE api_keys SET last_used_at = CURRENT_TIMESTAMP WHERE id = ?",
                        [&key.id],
                    );
                    Ok(Some((user, key)))
                } else {
                    Ok(None)
                }
            })
            .await
    }

    pub async fn list_api_keys(&self, user_id: &str) -> anyhow::Result<Vec<ApiKey>> {
        let user_id = user_id.to_string();
        self.db
            .with(move |c| {
                let mut stmt = c.prepare(
                    "SELECT id, user_id, key_hash, key_prefix, name, last_used_at, expires_at, created_at, revoked
                     FROM api_keys WHERE user_id = ? ORDER BY created_at DESC",
                )?;
                let rows = stmt
                    .query_map([&user_id], row_to_api_key)?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
    }

    pub async fn revoke_api_key(&self, key_id: &str) -> anyhow::Result<()> {
        let key_id = key_id.to_string();
        self.db
            .with(move |c| {
                c.execute("UPDATE api_keys SET revoked = 1 WHERE id = ?", [&key_id])?;
                Ok(())
            })
            .await
    }

    pub async fn delete_api_key(&self, key_id: &str) -> anyhow::Result<()> {
        let key_id = key_id.to_string();
        self.db
            .with(move |c| {
                c.execute("DELETE FROM api_keys WHERE id = ?", [&key_id])?;
                Ok(())
            })
            .await
    }

    // Sessions
    pub async fn create_session(
        &self,
        user_id: &str,
        user_agent: Option<&str>,
        ip: Option<&str>,
        ttl_hours: i64,
    ) -> anyhow::Result<(Session, String)> {
        let id = uuid::Uuid::new_v4().to_string();
        let (plaintext, hash) = generate_session_token();
        let now = Utc::now();
        let expires = now + chrono::Duration::hours(ttl_hours);
        let session = Session {
            id: id.clone(),
            user_id: user_id.to_string(),
            session_hash: hash.clone(),
            user_agent: user_agent.map(String::from),
            ip: ip.map(String::from),
            created_at: now,
            expires_at: expires,
            last_seen_at: now,
        };
        let id_c = session.id.clone();
        let user_id_c = session.user_id.clone();
        let hash_c = session.session_hash.clone();
        let ua_c = session.user_agent.clone();
        let ip_c = session.ip.clone();
        let exp_str = session.expires_at.to_rfc3339();
        self.db
            .with(move |c| {
                c.execute(
                    "INSERT INTO sessions (id, user_id, session_hash, user_agent, ip, expires_at) VALUES (?,?,?,?,?,?)",
                    rusqlite::params![id_c, user_id_c, hash_c, ua_c, ip_c, exp_str],
                )?;
                Ok(())
            })
            .await?;
        Ok((session, plaintext))
    }

    pub async fn get_session(&self, plaintext: &str) -> anyhow::Result<Option<(Session, User)>> {
        let hash = sha256_hex(plaintext);
        self.db
            .with(move |c| {
                let mut stmt = c.prepare(
                    "SELECT s.id, s.user_id, s.session_hash, s.user_agent, s.ip, s.created_at, s.expires_at, s.last_seen_at,
                            u.id, u.email, u.name, u.password_hash, u.role, u.metadata, u.created_at, u.last_login_at
                     FROM sessions s JOIN users u ON u.id = s.user_id
                     WHERE s.session_hash = ? AND s.expires_at > CURRENT_TIMESTAMP",
                )?;
                let mut rows = stmt.query([hash])?;
                if let Some(row) = rows.next()? {
                    let session = row_to_session(row)?;
                    let user = row_to_user_from_session(row)?;
                    let _ = c.execute(
                        "UPDATE sessions SET last_seen_at = CURRENT_TIMESTAMP WHERE id = ?",
                        [&session.id],
                    );
                    Ok(Some((session, user)))
                } else {
                    Ok(None)
                }
            })
            .await
    }

    pub async fn delete_session(&self, id: &str) -> anyhow::Result<()> {
        let id = id.to_string();
        self.db
            .with(move |c| {
                c.execute("DELETE FROM sessions WHERE id = ?", [&id])?;
                Ok(())
            })
            .await
    }

    pub async fn delete_user_sessions(&self, user_id: &str) -> anyhow::Result<()> {
        let user_id = user_id.to_string();
        self.db
            .with(move |c| {
                c.execute("DELETE FROM sessions WHERE user_id = ?", [&user_id])?;
                Ok(())
            })
            .await
    }

    pub async fn prune_expired_sessions(&self) -> anyhow::Result<usize> {
        self.db
            .with(|c| {
                let n = c.execute(
                    "DELETE FROM sessions WHERE expires_at < CURRENT_TIMESTAMP",
                    [],
                )?;
                Ok(n)
            })
            .await
    }

    // Token usage
    pub async fn record_usage(
        &self,
        user_id: &str,
        input_tokens: u32,
        output_tokens: u32,
        cost_usd: f64,
    ) -> anyhow::Result<()> {
        let user_id = user_id.to_string();
        let day = Utc::now().format("%Y-%m-%d").to_string();
        self.db
            .with(move |c| {
                c.execute(
                    "INSERT INTO token_usage (user_id, day, input_tokens, output_tokens, cost_usd, request_count)
                     VALUES (?, ?, ?, ?, ?, 1)
                     ON CONFLICT(user_id, day) DO UPDATE SET
                       input_tokens = input_tokens + excluded.input_tokens,
                       output_tokens = output_tokens + excluded.output_tokens,
                       cost_usd = cost_usd + excluded.cost_usd,
                       request_count = request_count + 1,
                       updated_at = CURRENT_TIMESTAMP",
                    rusqlite::params![user_id, day, input_tokens, output_tokens, cost_usd],
                )?;
                Ok(())
            })
            .await
    }

    pub async fn get_usage_today(&self, user_id: &str) -> anyhow::Result<(u32, u32, f64, u32)> {
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let user_id = user_id.to_string();
        self.db
            .with(move |c| {
                let mut stmt = c.prepare(
                    "SELECT input_tokens, output_tokens, cost_usd, request_count
                     FROM token_usage WHERE user_id = ? AND day = ?",
                )?;
                let mut rows = stmt.query(rusqlite::params![user_id, day])?;
                if let Some(row) = rows.next()? {
                    let input: i64 = row.get(0)?;
                    let output: i64 = row.get(1)?;
                    let cost: f64 = row.get(2)?;
                    let reqs: i64 = row.get(3)?;
                    Ok((input as u32, output as u32, cost, reqs as u32))
                } else {
                    Ok((0, 0, 0.0, 0))
                }
            })
            .await
    }

    pub async fn get_usage_summary(
        &self,
        user_id: &str,
        days: u32,
    ) -> anyhow::Result<Vec<(String, u32, u32, f64)>> {
        let user_id = user_id.to_string();
        self.db
            .with(move |c| {
                let mut stmt = c.prepare(
                    "SELECT day, input_tokens, output_tokens, cost_usd
                     FROM token_usage WHERE user_id = ? AND day >= date('now', ?)
                     ORDER BY day ASC",
                )?;
                let since = format!("-{} days", days);
                let rows = stmt
                    .query_map(rusqlite::params![user_id, since], |row| {
                        let day: String = row.get(0)?;
                        let input: i64 = row.get(1)?;
                        let output: i64 = row.get(2)?;
                        let cost: f64 = row.get(3)?;
                        Ok((day, input as u32, output as u32, cost))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
    }

    pub async fn get_global_usage_today(&self) -> anyhow::Result<(u32, u32, f64, u32)> {
        let day = Utc::now().format("%Y-%m-%d").to_string();
        self.db
            .with(move |c| {
                let mut stmt = c.prepare(
                    "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                            COALESCE(SUM(cost_usd),0.0), COALESCE(SUM(request_count),0)
                     FROM token_usage WHERE day = ?",
                )?;
                let mut rows = stmt.query([day])?;
                if let Some(row) = rows.next()? {
                    let input: i64 = row.get(0)?;
                    let output: i64 = row.get(1)?;
                    let cost: f64 = row.get(2)?;
                    let reqs: i64 = row.get(3)?;
                    Ok((input as u32, output as u32, cost, reqs as u32))
                } else {
                    Ok((0, 0, 0.0, 0))
                }
            })
            .await
    }
}

// ── row → struct helpers ──────────────────────────────────────────

fn row_to_user(row: &rusqlite::Row) -> rusqlite::Result<User> {
    let role_str: String = row.get(4)?;
    let metadata_str: String = row.get(5)?;
    let created_at_str: String = row.get(6)?;
    let last_login_at_str: Option<String> = row.get(7)?;
    Ok(User {
        id: row.get(0)?,
        email: row.get(1)?,
        name: row.get(2)?,
        password_hash: row.get(3)?,
        role: Role::parse(&role_str),
        metadata: serde_json::from_str(&metadata_str).unwrap_or(serde_json::json!({})),
        created_at: parse_dt(&created_at_str),
        last_login_at: last_login_at_str.as_deref().map(parse_dt),
    })
}

// When a JOIN row is in scope, the user columns are at offsets 9..17.
fn row_to_user_from_key(row: &rusqlite::Row) -> rusqlite::Result<User> {
    let role_str: String = row.get(13)?;
    let metadata_str: String = row.get(14)?;
    let created_at_str: String = row.get(15)?;
    let last_login_at_str: Option<String> = row.get(16)?;
    Ok(User {
        id: row.get(9)?,
        email: row.get(10)?,
        name: row.get(11)?,
        password_hash: row.get(12)?,
        role: Role::parse(&role_str),
        metadata: serde_json::from_str(&metadata_str).unwrap_or(serde_json::json!({})),
        created_at: parse_dt(&created_at_str),
        last_login_at: last_login_at_str.as_deref().map(parse_dt),
    })
}

fn row_to_user_from_session(row: &rusqlite::Row) -> rusqlite::Result<User> {
    let role_str: String = row.get(13)?;
    let metadata_str: String = row.get(14)?;
    let created_at_str: String = row.get(15)?;
    let last_login_at_str: Option<String> = row.get(16)?;
    Ok(User {
        id: row.get(9)?,
        email: row.get(10)?,
        name: row.get(11)?,
        password_hash: row.get(12)?,
        role: Role::parse(&role_str),
        metadata: serde_json::from_str(&metadata_str).unwrap_or(serde_json::json!({})),
        created_at: parse_dt(&created_at_str),
        last_login_at: last_login_at_str.as_deref().map(parse_dt),
    })
}

fn row_to_api_key(row: &rusqlite::Row) -> rusqlite::Result<ApiKey> {
    let last_used_str: Option<String> = row.get(5)?;
    let expires_str: Option<String> = row.get(6)?;
    let created_str: String = row.get(7)?;
    let revoked: i64 = row.get(8)?;
    Ok(ApiKey {
        id: row.get(0)?,
        user_id: row.get(1)?,
        key_hash: row.get(2)?,
        key_prefix: row.get(3)?,
        name: row.get(4)?,
        last_used_at: last_used_str.as_deref().map(parse_dt),
        expires_at: expires_str.as_deref().map(parse_dt),
        created_at: parse_dt(&created_str),
        revoked: revoked != 0,
    })
}

fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<Session> {
    let created_str: String = row.get(5)?;
    let expires_str: String = row.get(6)?;
    let last_seen_str: String = row.get(7)?;
    Ok(Session {
        id: row.get(0)?,
        user_id: row.get(1)?,
        session_hash: row.get(2)?,
        user_agent: row.get(3)?,
        ip: row.get(4)?,
        created_at: parse_dt(&created_str),
        expires_at: parse_dt(&expires_str),
        last_seen_at: parse_dt(&last_seen_str),
    })
}

fn parse_dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}
