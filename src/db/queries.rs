//! Typed SQL queries. Each function takes a `&Connection` and returns
//! parsed rows. Callers wrap in `Db::with(...)` to off the runtime.

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

/// Snapshot of a single request for the log writer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLog {
    pub id: String,
    pub tier: String,
    pub requested_model: Option<String>,
    pub routed_model: String,
    pub routed_provider: String,
    pub total_latency_ms: i64,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub cache_read_tokens: Option<u32>,
    pub cost_usd: Option<f64>,
    pub truncated: bool,
    pub fallback_count: u32,
    pub finished: bool,
    pub finish_reason: Option<String>,
    pub client_ip: Option<String>,
    pub user_id: Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptLog {
    pub request_id: String,
    pub attempt_number: u32,
    pub provider: String,
    pub model: String,
    pub outcome: String,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub latency_ms: i64,
    pub retry_wait_ms: Option<u64>,
}

/// Full request row for the Logs UI screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestRow {
    pub id: String,
    pub created_at: String,
    pub tier: String,
    pub routed_model: String,
    pub routed_provider: String,
    pub total_latency_ms: i64,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub cost_usd: Option<f64>,
    pub fallback_count: u32,
    pub finished: bool,
    pub finish_reason: Option<String>,
    pub client_ip: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LogFilter {
    pub limit: u32,
    pub tier: Option<String>,
    pub provider: Option<String>,
    pub finished: Option<bool>,
}

pub fn insert_request(conn: &Connection, log: &RequestLog) -> rusqlite::Result<()> {
    conn.execute(
        r#"INSERT INTO request_log
           (id, tier, requested_model, routed_model, routed_provider,
            total_latency_ms, input_tokens, output_tokens, cache_read_tokens,
            cost_usd, truncated, fallback_count, finished, finish_reason, client_ip,
            user_id, user_agent)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)"#,
        params![
            log.id,
            log.tier,
            log.requested_model,
            log.routed_model,
            log.routed_provider,
            log.total_latency_ms,
            log.input_tokens,
            log.output_tokens,
            log.cache_read_tokens,
            log.cost_usd,
            log.truncated as i32,
            log.fallback_count as i64,
            log.finished as i32,
            log.finish_reason,
            log.client_ip,
            log.user_id,
            log.user_agent,
        ],
    )?;
    Ok(())
}

pub fn update_request_final(
    conn: &Connection,
    id: &str,
    total_latency_ms: i64,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    cost_usd: Option<f64>,
    finished: bool,
    finish_reason: Option<String>,
) -> rusqlite::Result<()> {
    conn.execute(
        r#"UPDATE request_log
           SET total_latency_ms = ?2,
               input_tokens = ?3,
               output_tokens = ?4,
               cost_usd = ?5,
               finished = ?6,
               finish_reason = ?7
           WHERE id = ?1"#,
        params![
            id,
            total_latency_ms,
            input_tokens,
            output_tokens,
            cost_usd,
            finished as i32,
            finish_reason
        ],
    )?;
    Ok(())
}

pub fn insert_attempt(conn: &Connection, attempt: &AttemptLog) -> rusqlite::Result<()> {
    conn.execute(
        r#"INSERT INTO attempt_log
           (request_id, attempt_number, provider, model, outcome,
            error_code, error_message, latency_ms, retry_wait_ms)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)"#,
        params![
            attempt.request_id,
            attempt.attempt_number as i64,
            attempt.provider,
            attempt.model,
            attempt.outcome,
            attempt.error_code,
            attempt.error_message,
            attempt.latency_ms,
            attempt.retry_wait_ms,
        ],
    )?;
    Ok(())
}

pub fn list_requests(conn: &Connection, filter: &LogFilter) -> rusqlite::Result<Vec<RequestRow>> {
    let limit = filter.limit.max(1).min(500) as i64;
    let mut sql = String::from(
        "SELECT id, created_at, tier, routed_model, routed_provider,
                total_latency_ms, input_tokens, output_tokens, cost_usd,
                fallback_count, finished, finish_reason, client_ip
         FROM request_log WHERE 1=1",
    );
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(t) = &filter.tier {
        sql.push_str(" AND tier = ?");
        args.push(Box::new(t.clone()));
    }
    if let Some(p) = &filter.provider {
        sql.push_str(" AND routed_provider = ?");
        args.push(Box::new(p.clone()));
    }
    if let Some(f) = filter.finished {
        sql.push_str(" AND finished = ?");
        args.push(Box::new(f as i32));
    }
    sql.push_str(" ORDER BY created_at DESC LIMIT ?");
    args.push(Box::new(limit));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(args.iter()), row_to_request_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn count_requests(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM request_log", [], |r| {
        r.get::<_, i64>(0)
    })
}

pub fn attempts_for_request(
    conn: &Connection,
    request_id: &str,
) -> rusqlite::Result<Vec<AttemptLog>> {
    let mut stmt = conn.prepare(
        "SELECT request_id, attempt_number, provider, model, outcome,
                error_code, error_message, latency_ms, retry_wait_ms
         FROM attempt_log WHERE request_id = ?1 ORDER BY attempt_number ASC",
    )?;
    let rows = stmt
        .query_map(params![request_id], |r| {
            Ok(AttemptLog {
                request_id: r.get(0)?,
                attempt_number: r.get::<_, i64>(1)? as u32,
                provider: r.get(2)?,
                model: r.get(3)?,
                outcome: r.get(4)?,
                error_code: r.get(5)?,
                error_message: r.get(6)?,
                latency_ms: r.get(7)?,
                retry_wait_ms: r.get::<_, Option<i64>>(8)?.map(|x| x as u64),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn row_to_request_row(r: &Row) -> rusqlite::Result<RequestRow> {
    Ok(RequestRow {
        id: r.get(0)?,
        created_at: r.get(1)?,
        tier: r.get(2)?,
        routed_model: r.get(3)?,
        routed_provider: r.get(4)?,
        total_latency_ms: r.get(5)?,
        input_tokens: r.get::<_, Option<i64>>(6)?.map(|x| x as u32),
        output_tokens: r.get::<_, Option<i64>>(7)?.map(|x| x as u32),
        cost_usd: r.get(8)?,
        fallback_count: r.get::<_, i64>(9)? as u32,
        finished: r.get::<_, i64>(10)? != 0,
        finish_reason: r.get(11)?,
        client_ip: r.get(12)?,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyTotal {
    pub day: String,
    pub count: i64,
    pub cost: f64,
    pub tokens_in: i64,
    pub tokens_out: i64,
}

pub fn daily_totals(conn: &Connection, days: u32) -> rusqlite::Result<Vec<DailyTotal>> {
    let mut stmt = conn.prepare(
        "SELECT date(created_at) AS day,
                COUNT(*) AS count,
                COALESCE(SUM(cost_usd), 0.0) AS cost,
                COALESCE(SUM(input_tokens), 0) AS tokens_in,
                COALESCE(SUM(output_tokens), 0) AS tokens_out
         FROM request_log
         WHERE created_at >= datetime('now', ?1)
         GROUP BY day ORDER BY day DESC",
    )?;
    let rows = stmt
        .query_map(params![format!("-{} days", days as i64)], |r| {
            Ok(DailyTotal {
                day: r.get(0)?,
                count: r.get(1)?,
                cost: r.get(2)?,
                tokens_in: r.get(3)?,
                tokens_out: r.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Reset all data (used by the UI admin "clear logs" action).
pub fn clear_all(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM attempt_log", [])?;
    conn.execute("DELETE FROM request_log", [])?;
    Ok(())
}

/// Provider health row (mirrors HealthRegistry; persisted for restart survival).
pub fn upsert_provider_health(
    conn: &Connection,
    provider_id: &str,
    status: &str,
    consecutive_failures: u32,
) -> rusqlite::Result<()> {
    conn.execute(
        r#"INSERT INTO provider_health (provider_id, status, consecutive_failures, updated_at)
           VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP)
           ON CONFLICT(provider_id) DO UPDATE SET
             status = excluded.status,
             consecutive_failures = excluded.consecutive_failures,
             updated_at = CURRENT_TIMESTAMP"#,
        params![provider_id, status, consecutive_failures as i64],
    )?;
    Ok(())
}

pub fn read_provider_health(
    conn: &Connection,
    provider_id: &str,
) -> rusqlite::Result<Option<(String, u32)>> {
    conn.query_row(
        "SELECT status, consecutive_failures FROM provider_health WHERE provider_id = ?1",
        params![provider_id],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u32)),
    )
    .optional()
}
