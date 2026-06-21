//! Fire-and-forget request logging. The hot path never blocks on a
//! SQLite write — we spawn the actual write into a blocking task and
//! drop the result if it fails. If the DB is wedged, requests still
//! flow.

use crate::db::queries::{AttemptLog, RequestLog};
use crate::db::Db;

pub fn log_request(db: &Db, log: RequestLog) {
    let db = db.clone();
    tokio::spawn(async move {
        let _ = db
            .with(move |conn| {
                crate::db::queries::insert_request(conn, &log)
                    .map_err(|e| anyhow::anyhow!("log insert failed: {e}"))
            })
            .await;
    });
}

#[allow(clippy::too_many_arguments)]
pub fn update_request_final(
    db: &Db,
    id: String,
    total_latency_ms: i64,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    cost_usd: Option<f64>,
    finished: bool,
    finish_reason: Option<String>,
) {
    let db = db.clone();
    tokio::spawn(async move {
        let _ = db
            .with(move |conn| {
                crate::db::queries::update_request_final(
                    conn,
                    &id,
                    total_latency_ms,
                    input_tokens,
                    output_tokens,
                    cost_usd,
                    finished,
                    finish_reason,
                )
                .map_err(|e| anyhow::anyhow!("log update failed: {e}"))
            })
            .await;
    });
}

pub fn log_attempt(db: &Db, attempt: AttemptLog) {
    let db = db.clone();
    tokio::spawn(async move {
        let _ = db
            .with(move |conn| {
                crate::db::queries::insert_attempt(conn, &attempt)
                    .map_err(|e| anyhow::anyhow!("attempt insert failed: {e}"))
            })
            .await;
    });
}
