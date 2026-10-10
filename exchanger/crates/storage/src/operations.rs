//! Outbox действий юзербота (`operations`, CLAUDE.md п. 4–6).
//!
//! Жизнь операции: `pending` (вставлена в одной транзакции с переходом заявки) →
//! `dispatched` (коммит ДО действия юзербота) → `succeeded` / `failed` / `unknown`.
//! - `release` — действие не выполнялось (`BotError`): назад в `pending`, попытка не считается;
//! - `retry` — исход неизвестен, но повтор безопасен (только `activate_check`): в `pending`,
//!   попытка считается;
//! - выводящие деньги операции — одна попытка (CHECK в схеме); их `unknown` разбирает сверка.

use chrono::{DateTime, Utc};
use domain::fsm::{MoneyRole, OpKind};
use domain::{Asset, Decimal, Platform};
use serde_json::Value;
use sqlx::PgConnection;

use crate::error::StorageError;

/// Статус операции (`operations.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpStatus {
    Pending,
    Dispatched,
    Succeeded,
    Failed,
    Unknown,
    Cancelled,
}

impl OpStatus {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            OpStatus::Pending => "pending",
            OpStatus::Dispatched => "dispatched",
            OpStatus::Succeeded => "succeeded",
            OpStatus::Failed => "failed",
            OpStatus::Unknown => "unknown",
            OpStatus::Cancelled => "cancelled",
        }
    }

    fn parse(s: &str) -> Result<Self, StorageError> {
        [
            OpStatus::Pending,
            OpStatus::Dispatched,
            OpStatus::Succeeded,
            OpStatus::Failed,
            OpStatus::Unknown,
            OpStatus::Cancelled,
        ]
        .into_iter()
        .find(|st| st.as_db_str() == s)
        .ok_or_else(|| StorageError::corrupt("operations.status", s))
    }
}

/// Новая операция заявки.
#[derive(Debug, Clone)]
pub struct NewOperation<'a> {
    pub order_id: i64,
    pub kind: OpKind,
    pub role: MoneyRole,
    pub platform: Platform,
    pub wallet_account_id: i16,
    /// `ord-<id>-<вид>-<n>`; пишется и в метку операции в «Избранном».
    pub idempotency_key: &'a str,
    pub start_param: Option<&'a str>,
    pub amount: Option<Decimal>,
    pub asset: Asset,
    /// `random_id` Telegram, назначенный заранее (хранится в `request`).
    pub random_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRow {
    pub id: i64,
    pub order_id: Option<i64>,
    pub kind: OpKind,
    pub role: MoneyRole,
    pub platform: Platform,
    pub wallet_account_id: Option<i16>,
    pub status: OpStatus,
    pub idempotency_key: String,
    pub start_param: Option<String>,
    pub amount: Option<Decimal>,
    pub asset: Option<Asset>,
    pub random_id: i64,
    pub attempts: i32,
    pub max_attempts: i32,
    pub external_id: Option<String>,
    pub dispatched_at: Option<DateTime<Utc>>,
}

struct RawRow {
    id: i64,
    order_id: Option<i64>,
    kind: String,
    money_role: String,
    platform: String,
    wallet_account_id: Option<i16>,
    status: String,
    idempotency_key: String,
    start_param: Option<String>,
    amount: Option<Decimal>,
    asset: Option<String>,
    request: Value,
    attempts: i32,
    max_attempts: i32,
    external_id: Option<String>,
    dispatched_at: Option<DateTime<Utc>>,
}

impl TryFrom<RawRow> for OperationRow {
    type Error = StorageError;

    fn try_from(r: RawRow) -> Result<Self, StorageError> {
        let random_id = r
            .request
            .get("random_id")
            .and_then(Value::as_i64)
            .ok_or_else(|| StorageError::corrupt("operations.request.random_id", &r.request))?;
        Ok(Self {
            id: r.id,
            order_id: r.order_id,
            kind: OpKind::from_db_str(&r.kind)
                .ok_or_else(|| StorageError::corrupt("operations.kind", &r.kind))?,
            role: MoneyRole::from_db_str(&r.money_role)
                .ok_or_else(|| StorageError::corrupt("operations.money_role", &r.money_role))?,
            platform: domain::Platform::from_db_name(&r.platform)
                .ok_or_else(|| StorageError::corrupt("operations.platform", &r.platform))?,
            wallet_account_id: r.wallet_account_id,
            status: OpStatus::parse(&r.status)?,
            idempotency_key: r.idempotency_key,
            start_param: r.start_param,
            amount: r.amount,
            asset: r
                .asset
                .map(|a| {
                    a.parse()
                        .map_err(|_| StorageError::corrupt("operations.asset", &a))
                })
                .transpose()?,
            random_id,
            attempts: r.attempts,
            max_attempts: r.max_attempts,
            external_id: r.external_id,
            dispatched_at: r.dispatched_at,
        })
    }
}

/// Вставить операцию (в транзакции перехода заявки). Выводящие деньги — одна попытка.
pub async fn insert(conn: &mut PgConnection, op: &NewOperation<'_>) -> Result<i64, StorageError> {
    let max_attempts: i32 = if op.kind.retry_is_safe() { 8 } else { 1 };
    let request = serde_json::json!({ "random_id": op.random_id });
    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO operations (order_id, kind, money_role, platform, wallet_account_id,
                                idempotency_key, start_param, amount, asset, request, max_attempts)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        RETURNING id
        "#,
        op.order_id,
        op.kind.as_db_str(),
        op.role.as_db_str(),
        op.platform.db_name(),
        op.wallet_account_id,
        op.idempotency_key,
        op.start_param,
        op.amount,
        op.asset.code(),
        request,
        max_attempts,
    )
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query!("SELECT pg_notify('exch_operations', $1)", id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(id)
}

/// Сколько операций этого вида уже было у заявки (для номера в ключе).
pub async fn count_for_order(
    conn: &mut PgConnection,
    order_id: i64,
    kind: OpKind,
) -> Result<i64, StorageError> {
    let n = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM operations WHERE order_id = $1 AND kind = $2"#,
        order_id,
        kind.as_db_str(),
    )
    .fetch_one(conn)
    .await?;
    Ok(n)
}

/// Забрать до `limit` созревших операций: `pending` → `dispatched`, `attempts + 1`.
/// Коммитить сразу: действие юзербота — только после коммита.
pub async fn claim_due(
    conn: &mut PgConnection,
    limit: i64,
) -> Result<Vec<OperationRow>, StorageError> {
    let rows = sqlx::query_as!(
        RawRow,
        r#"
        UPDATE operations o
           SET status = 'dispatched', attempts = o.attempts + 1, dispatched_at = now()
          FROM (SELECT id FROM operations
                 WHERE status = 'pending' AND next_attempt_at <= now()
                 ORDER BY next_attempt_at, id
                 LIMIT $1
                 FOR UPDATE SKIP LOCKED) due
         WHERE o.id = due.id
        RETURNING o.id, o.order_id, o.kind, o.money_role, o.platform, o.wallet_account_id,
                  o.status, o.idempotency_key, o.start_param, o.amount, o.asset, o.request,
                  o.attempts, o.max_attempts, o.external_id, o.dispatched_at
        "#,
        limit,
    )
    .fetch_all(conn)
    .await?;
    let mut ops = rows
        .into_iter()
        .map(OperationRow::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    ops.sort_by_key(|o| o.id);
    Ok(ops)
}

/// Операции в работе или с неизвестным исходом (восстановление после рестарта, сверка).
pub async fn in_flight(conn: &mut PgConnection) -> Result<Vec<OperationRow>, StorageError> {
    let rows = sqlx::query_as!(
        RawRow,
        r#"
        SELECT id, order_id, kind, money_role, platform, wallet_account_id, status,
               idempotency_key, start_param, amount, asset, request, attempts, max_attempts,
               external_id, dispatched_at
          FROM operations
         WHERE status IN ('dispatched', 'unknown')
         ORDER BY id
        "#
    )
    .fetch_all(conn)
    .await?;
    rows.into_iter().map(OperationRow::try_from).collect()
}

pub async fn get(conn: &mut PgConnection, id: i64) -> Result<OperationRow, StorageError> {
    let row = sqlx::query_as!(
        RawRow,
        r#"
        SELECT id, order_id, kind, money_role, platform, wallet_account_id, status,
               idempotency_key, start_param, amount, asset, request, attempts, max_attempts,
               external_id, dispatched_at
          FROM operations WHERE id = $1
        "#,
        id
    )
    .fetch_optional(conn)
    .await?
    .ok_or(StorageError::NotFound(id))?;
    OperationRow::try_from(row)
}

/// Все операции заявки по порядку.
pub async fn for_order(
    conn: &mut PgConnection,
    order_id: i64,
) -> Result<Vec<OperationRow>, StorageError> {
    let rows = sqlx::query_as!(
        RawRow,
        r#"
        SELECT id, order_id, kind, money_role, platform, wallet_account_id, status,
               idempotency_key, start_param, amount, asset, request, attempts, max_attempts,
               external_id, dispatched_at
          FROM operations WHERE order_id = $1 ORDER BY id
        "#,
        order_id
    )
    .fetch_all(conn)
    .await?;
    rows.into_iter().map(OperationRow::try_from).collect()
}

/// Итог операции. Переход из `dispatched` или `unknown`; из других статусов — ошибка
/// (итог уже записан другим обработчиком).
pub async fn finish(
    conn: &mut PgConnection,
    id: i64,
    status: OpStatus,
    response: &Value,
    external_id: Option<&str>,
    error: Option<&str>,
) -> Result<(), StorageError> {
    debug_assert!(matches!(
        status,
        OpStatus::Succeeded | OpStatus::Failed | OpStatus::Unknown
    ));
    let done = matches!(status, OpStatus::Succeeded | OpStatus::Failed);
    let updated = sqlx::query!(
        r#"
        UPDATE operations
           SET status = $2, response = $3, external_id = COALESCE($4, external_id),
               last_error = $5, finished_at = CASE WHEN $6 THEN now() ELSE finished_at END
         WHERE id = $1 AND status IN ('dispatched', 'unknown')
        "#,
        id,
        status.as_db_str(),
        response,
        external_id,
        error,
        done,
    )
    .execute(conn)
    .await?
    .rows_affected();
    if updated == 1 {
        Ok(())
    } else {
        Err(StorageError::OperationState(id))
    }
}

/// Действие не выполнялось: назад в `pending` на `at`, попытка не считается.
pub async fn release(
    conn: &mut PgConnection,
    id: i64,
    at: DateTime<Utc>,
    error: &str,
) -> Result<(), StorageError> {
    let updated = sqlx::query!(
        r#"
        UPDATE operations
           SET status = 'pending', attempts = attempts - 1, next_attempt_at = $2, last_error = $3
         WHERE id = $1 AND status = 'dispatched'
        "#,
        id,
        at,
        error,
    )
    .execute(conn)
    .await?
    .rows_affected();
    if updated == 1 {
        Ok(())
    } else {
        Err(StorageError::OperationState(id))
    }
}

/// Повторить операцию с безопасным повтором (`activate_check`) на `at`; попытка считается.
/// `false` — попытки кончились, операция осталась как была.
pub async fn retry(
    conn: &mut PgConnection,
    id: i64,
    at: DateTime<Utc>,
    error: &str,
) -> Result<bool, StorageError> {
    let updated = sqlx::query!(
        r#"
        UPDATE operations
           SET status = 'pending', next_attempt_at = $2, last_error = $3
         WHERE id = $1 AND status IN ('dispatched', 'unknown')
           AND kind = 'activate_check' AND attempts < max_attempts
        "#,
        id,
        at,
        error,
    )
    .execute(conn)
    .await?
    .rows_affected();
    Ok(updated == 1)
}
