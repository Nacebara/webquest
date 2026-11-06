//! Журнал действий персонала (`admin_audit`, SPEC §7.4): только дописывается (триггер).
//!
//! Запись делается в той же транзакции, что и само изменение: изменения без записи в журнале
//! не бывает.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgConnection;

use crate::error::StorageError;

#[derive(Debug, Clone, PartialEq)]
pub struct NewAudit<'a> {
    pub staff_id: i64,
    /// `direction.set_fee`, `order.retry_payout`, `user.ban`, `settings.emoji_style` …
    pub action: &'a str,
    pub target_type: Option<&'a str>,
    pub target_id: Option<&'a str>,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub reason: Option<&'a str>,
    pub double_confirmed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AuditRow {
    pub id: i64,
    pub staff_id: i64,
    pub staff_name: Option<String>,
    pub action: String,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub reason: Option<String>,
    pub double_confirmed: bool,
    pub at: DateTime<Utc>,
}

pub async fn record(conn: &mut PgConnection, a: &NewAudit<'_>) -> Result<i64, StorageError> {
    Ok(sqlx::query_scalar!(
        r#"
        INSERT INTO admin_audit (staff_id, action, target_type, target_id, before, after, reason,
                                 double_confirmed)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING id
        "#,
        a.staff_id,
        a.action,
        a.target_type,
        a.target_id,
        a.before,
        a.after,
        a.reason,
        a.double_confirmed,
    )
    .fetch_one(conn)
    .await?)
}

/// Последние записи, новые сверху. `staff_id` — только этого сотрудника, `target` — только
/// по этому объекту (`("order", "15")`).
pub async fn recent(
    conn: &mut PgConnection,
    limit: i64,
    staff_id: Option<i64>,
    target: Option<(&str, &str)>,
) -> Result<Vec<AuditRow>, StorageError> {
    let (target_type, target_id) = target.unzip();
    let rows = sqlx::query!(
        r#"
        SELECT a.id, a.staff_id, s.display_name AS "staff_name?", a.action, a.target_type,
               a.target_id, a.before, a.after, a.reason, a.double_confirmed, a.at
          FROM admin_audit a LEFT JOIN staff s ON s.tg_user_id = a.staff_id
         WHERE ($2::BIGINT IS NULL OR a.staff_id = $2)
           AND ($3::TEXT IS NULL OR (a.target_type = $3 AND a.target_id = $4))
         ORDER BY a.id DESC
         LIMIT $1
        "#,
        limit,
        staff_id,
        target_type,
        target_id,
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| AuditRow {
            id: r.id,
            staff_id: r.staff_id,
            staff_name: r.staff_name,
            action: r.action,
            target_type: r.target_type,
            target_id: r.target_id,
            before: r.before,
            after: r.after,
            reason: r.reason,
            double_confirmed: r.double_confirmed,
            at: r.at,
        })
        .collect())
}
