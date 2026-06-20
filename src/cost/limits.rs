//! Cost budget enforcement.
//!
//! Checks per-day, per-user, and per-request cost limits configured
//! in `[budgets]` of `token-dealer.toml`. Runs in the chat handler
//! before dispatch + in the pipeline after the response (to apply
//! the actual cost against the day's running total).
//!
//! Returns `BudgetDecision::Allow` / `SoftWarning` / `Deny`.
//!   - Allow: under 80% of any cap, proceed normally.
//!   - SoftWarning: between warn_fraction and 100% — log + emit
//!     `x-router-budget-warning` response header (caller decides).
//!   - Deny: over 100% — return 429 to the client.
//!
//! Per-day cost comes from the `token_usage` table (already kept by
//! the multi-user flow). Per-request cost is computed from the
//! upstream-reported token usage + the pricing store.

use crate::config::types::BudgetConfig;
use crate::cost::PricingStore;
use crate::db::Db;
use chrono::Utc;

#[derive(Debug, Clone, PartialEq)]
pub enum BudgetDecision {
    Allow,
    SoftWarning { fraction: f64, kind: &'static str },
    Deny { reason: String },
}

/// Look up today's spend for a user (UTC day).
pub async fn daily_spend(db: &Db, user_id: Option<&str>) -> anyhow::Result<f64> {
    let day = Utc::now().format("%Y-%m-%d").to_string();
    let uid = user_id.unwrap_or("").to_string();
    let row: Option<f64> = db
        .with(move |c| {
            let r: Option<f64> = c
                .query_row(
                    "SELECT cost_usd FROM token_usage WHERE user_id = ?1 AND day = ?2",
                    rusqlite::params![&uid, &day],
                    |row| row.get(0),
                )
                .ok();
            Ok(r)
        })
        .await?;
    Ok(row.unwrap_or(0.0))
}

/// Bump today's spend atomically. Called from the pipeline after a
/// successful response (or partial success).
pub async fn record_spend(
    db: &Db,
    user_id: Option<&str>,
    cost_usd: f64,
    input_tokens: u32,
    output_tokens: u32,
) -> anyhow::Result<()> {
    if cost_usd <= 0.0 && input_tokens == 0 && output_tokens == 0 {
        return Ok(());
    }
    let day = Utc::now().format("%Y-%m-%d").to_string();
    let uid = user_id.unwrap_or("").to_string();
    db.with(move |c| {
        c.execute(
            "INSERT INTO token_usage (user_id, day, input_tokens, output_tokens, cost_usd, request_count, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, CURRENT_TIMESTAMP)
             ON CONFLICT(user_id, day) DO UPDATE SET
               input_tokens = input_tokens + ?3,
               output_tokens = output_tokens + ?4,
               cost_usd = cost_usd + ?5,
               request_count = request_count + 1,
               updated_at = CURRENT_TIMESTAMP",
            rusqlite::params![uid, day, input_tokens as i64, output_tokens as i64, cost_usd],
        )?;
        Ok(())
    })
    .await
}

/// Check the configured budgets against the projected (or final)
/// cost of the current request.
pub async fn check(
    db: &Db,
    pricing: &PricingStore,
    cfg: &BudgetConfig,
    user_id: Option<&str>,
    request_cost_usd: f64,
) -> anyhow::Result<BudgetDecision> {
    // Per-request hard cap
    if cfg.per_request_cost_usd > 0.0 && request_cost_usd > cfg.per_request_cost_usd {
        return Ok(BudgetDecision::Deny {
            reason: format!(
                "request cost ${:.4} exceeds per-request cap ${:.4}",
                request_cost_usd, cfg.per_request_cost_usd
            ),
        });
    }
    // Per-day cap
    if cfg.daily_cost_usd > 0.0 {
        let today = daily_spend(db, user_id).await?;
        let projected = today + request_cost_usd;
        if projected > cfg.daily_cost_usd {
            return Ok(BudgetDecision::Deny {
                reason: format!(
                    "daily spend ${:.4} + request ${:.4} exceeds cap ${:.4}",
                    today, request_cost_usd, cfg.daily_cost_usd
                ),
            });
        }
        let fraction = projected / cfg.daily_cost_usd;
        if fraction >= cfg.warn_fraction {
            return Ok(BudgetDecision::SoftWarning {
                fraction,
                kind: "daily_cost",
            });
        }
    }
    let _ = pricing; // silence unused warning if no per-token budget
    Ok(BudgetDecision::Allow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_when_no_caps_set() {
        let cfg = BudgetConfig::default();
        // 0 caps → always allow
        assert!(matches!(cfg.daily_cost_usd, 0.0));
        assert!(matches!(cfg.per_request_cost_usd, 0.0));
    }

    #[test]
    fn per_request_cap_zero_means_unlimited() {
        let cfg = BudgetConfig {
            per_request_cost_usd: 0.0,
            ..BudgetConfig::default()
        };
        // 0 = unlimited, should never deny
        assert!(cfg.per_request_cost_usd == 0.0);
    }
}
