//! Выборки и изменения для пульта: очередь ручного разбора, карточки заявки и клиента,
//! сводка, баны, пороги резерва (SPEC §7.3).
//!
//! Только чтение и изменения, не трогающие денег. Решения по деньгам заявки — в
//! `engine::operator` (там же, в одной транзакции, запись в `admin_audit`).

use chrono::{DateTime, Utc};
use domain::fsm::OrderState;
use domain::{Asset, Decimal, Direction, Platform};
use serde_json::Value;
use sqlx::PgConnection;

use crate::error::StorageError;

fn parse<T: std::str::FromStr>(field: &'static str, value: &str) -> Result<T, StorageError> {
    value
        .parse()
        .map_err(|_| StorageError::corrupt(field, value))
}

// ---------------------------------------------------------------------------------------------
// Очередь разбора

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueItem {
    pub id: i64,
    pub public_id: String,
    pub user_id: i64,
    pub direction: Direction,
    pub asset: Asset,
    pub intake_amount: Option<Decimal>,
    pub payout_amount: Option<Decimal>,
    pub manual_reason: Option<String>,
    /// Когда заявка пришла на разбор (последний переход в `MANUAL_REVIEW`).
    pub since: DateTime<Utc>,
    /// Кто взял (`staff:<id>` из журнала аудита), если взяли.
    pub taken_by: Option<i64>,
}

/// Заявки на ручном разборе, старые сверху.
pub async fn review_queue(conn: &mut PgConnection) -> Result<Vec<QueueItem>, StorageError> {
    let rows = sqlx::query!(
        r#"
        SELECT o.id, o.public_id, o.user_id, o.direction, o.asset, o.intake_amount,
               o.payout_amount, o.manual_reason,
               COALESCE((SELECT max(t.at) FROM order_transitions t
                          WHERE t.order_id = o.id AND t.to_state = 'MANUAL_REVIEW'),
                        o.updated_at) AS "since!",
               (SELECT a.staff_id FROM admin_audit a
                 WHERE a.target_type = 'order' AND a.target_id = o.id::TEXT
                   AND a.action = 'order.take'
                 ORDER BY a.id DESC LIMIT 1) AS taken_by
          FROM orders o
         WHERE o.state = 'MANUAL_REVIEW'
         ORDER BY 9
        "#
    )
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|r| {
            Ok(QueueItem {
                id: r.id,
                public_id: r.public_id,
                user_id: r.user_id,
                direction: parse("orders.direction", &r.direction)?,
                asset: parse("orders.asset", &r.asset)?,
                intake_amount: r.intake_amount,
                payout_amount: r.payout_amount,
                manual_reason: r.manual_reason,
                since: r.since,
                taken_by: r.taken_by,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Поиск и карточка заявки

/// Найти заявку: внутренний номер, публичный (`A1B2C3D4E5`), чек или счёт клиента
/// (`CQ…`, `t_…`, `IV…`, `inv_…`) — последняя заявка с ним.
pub async fn find_order(conn: &mut PgConnection, query: &str) -> Result<Option<i64>, StorageError> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(None);
    }
    let id: Option<i64> = q.trim_start_matches('#').parse().ok();
    Ok(sqlx::query_scalar!(
        r#"
        SELECT id FROM orders
         WHERE id = $1 OR public_id = upper($2) OR intake_start_param = $2 OR invoice_start_param = $2
         ORDER BY (id = $1) DESC, created_at DESC
         LIMIT 1
        "#,
        id,
        q
    )
    .fetch_optional(conn)
    .await?)
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransitionRow {
    pub from: Option<String>,
    pub to: String,
    pub actor: String,
    pub reason: Option<String>,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OpLine {
    pub id: i64,
    pub kind: String,
    pub status: String,
    pub key: String,
    pub amount: Option<Decimal>,
    pub asset: Option<String>,
    pub external_id: Option<String>,
    pub last_error: Option<String>,
    pub response: Option<Value>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PostingLine {
    pub kind: String,
    pub memo: Option<String>,
    pub created_by: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WalletMessageLine {
    pub platform: Option<String>,
    pub outgoing: bool,
    pub text: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderCard {
    pub id: i64,
    pub public_id: String,
    pub user_id: i64,
    pub direction: Direction,
    pub asset: Asset,
    pub state: OrderState,
    pub version: i32,
    pub intake_platform: Platform,
    pub intake_start_param: Option<String>,
    pub intake_amount: Option<Decimal>,
    pub intake_asset: Option<String>,
    pub invoice_start_param: Option<String>,
    pub invoice_amount: Option<Decimal>,
    pub invoice_fiat: Option<String>,
    pub invoice_fiat_amount: Option<Decimal>,
    pub quote_amount_in: Option<Decimal>,
    pub fee_pct_applied: Option<Decimal>,
    pub fee_amount: Option<Decimal>,
    pub payout_amount: Option<Decimal>,
    pub refund_amount: Option<Decimal>,
    pub payout_check_url: Option<String>,
    pub failure_code: Option<String>,
    pub manual_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub transitions: Vec<TransitionRow>,
    pub operations: Vec<OpLine>,
    pub postings: Vec<PostingLine>,
    pub messages: Vec<WalletMessageLine>,
}

impl OrderCard {
    /// Приход по заявке проведён в леджере (деньги клиента у нас).
    pub fn intake_posted(&self) -> bool {
        self.postings.iter().any(|p| p.kind == "intake")
    }
}

pub async fn order_card(conn: &mut PgConnection, id: i64) -> Result<OrderCard, StorageError> {
    let o = sqlx::query!(
        r#"
        SELECT id, public_id, user_id, direction, asset, state, version, intake_platform,
               intake_start_param, intake_amount, intake_asset, invoice_start_param,
               invoice_amount, invoice_fiat, invoice_fiat_amount, quote_amount_in,
               fee_pct_applied, fee_amount, payout_amount, refund_amount, payout_check_url,
               failure_code, manual_reason, created_at, finished_at
          FROM orders WHERE id = $1
        "#,
        id
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(StorageError::NotFound(id))?;

    let transitions = sqlx::query_as!(
        TransitionRow,
        r#"
        SELECT from_state AS "from", to_state AS "to", actor, reason, at
          FROM order_transitions WHERE order_id = $1 ORDER BY id
        "#,
        id
    )
    .fetch_all(&mut *conn)
    .await?;
    let operations = sqlx::query_as!(
        OpLine,
        r#"
        SELECT id, kind, status, idempotency_key AS key, amount, asset, external_id, last_error,
               response, created_at
          FROM operations WHERE order_id = $1 ORDER BY id
        "#,
        id
    )
    .fetch_all(&mut *conn)
    .await?;
    let postings = sqlx::query_as!(
        PostingLine,
        r#"
        SELECT kind, memo, created_by, created_at AS at
          FROM ledger_transactions WHERE order_id = $1 ORDER BY id
        "#,
        id
    )
    .fetch_all(&mut *conn)
    .await?;
    let messages = sqlx::query!(
        r#"
        SELECT m.entities->>'via_bot' AS via_bot, m.is_outgoing, m.text, m.received_at
          FROM wallet_messages m JOIN operations op ON op.id = m.operation_id
         WHERE op.order_id = $1
         ORDER BY m.id DESC
         LIMIT 12
        "#,
        id
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .rev()
    .map(|m| WalletMessageLine {
        platform: m.via_bot,
        outgoing: m.is_outgoing,
        text: m.text,
        at: m.received_at,
    })
    .collect();

    Ok(OrderCard {
        id: o.id,
        public_id: o.public_id,
        user_id: o.user_id,
        direction: parse("orders.direction", &o.direction)?,
        asset: parse("orders.asset", &o.asset)?,
        state: parse("orders.state", &o.state)?,
        version: o.version,
        intake_platform: Platform::from_db_name(&o.intake_platform)
            .ok_or_else(|| StorageError::corrupt("orders.intake_platform", &o.intake_platform))?,
        intake_start_param: o.intake_start_param,
        intake_amount: o.intake_amount,
        intake_asset: o.intake_asset,
        invoice_start_param: o.invoice_start_param,
        invoice_amount: o.invoice_amount,
        invoice_fiat: o.invoice_fiat,
        invoice_fiat_amount: o.invoice_fiat_amount,
        quote_amount_in: o.quote_amount_in,
        fee_pct_applied: o.fee_pct_applied,
        fee_amount: o.fee_amount,
        payout_amount: o.payout_amount,
        refund_amount: o.refund_amount,
        payout_check_url: o.payout_check_url,
        failure_code: o.failure_code,
        manual_reason: o.manual_reason,
        created_at: o.created_at,
        finished_at: o.finished_at,
        transitions,
        operations,
        postings,
        messages,
    })
}

// ---------------------------------------------------------------------------------------------
// Клиент

#[derive(Debug, Clone, PartialEq)]
pub struct UserCard {
    pub tg_user_id: i64,
    pub username: Option<String>,
    pub first_name: Option<String>,
    pub is_premium: Option<bool>,
    pub daily_limit_usd: Option<Decimal>,
    pub risk_note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub first_completed_at: Option<DateTime<Utc>>,
    pub completed: i64,
    pub refunded: i64,
    pub in_review: i64,
    pub live: i64,
    /// Оборот завершённых заявок (сумма прихода).
    pub turnover: Decimal,
    pub fees: Decimal,
    pub ban: Option<BanRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BanRow {
    pub reason: String,
    pub banned_by: i64,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Клиент по id или `@username`.
pub async fn find_user(conn: &mut PgConnection, query: &str) -> Result<Option<i64>, StorageError> {
    let q = query.trim();
    if let Ok(id) = q.parse::<i64>() {
        return Ok(
            sqlx::query_scalar!("SELECT tg_user_id FROM users WHERE tg_user_id = $1", id)
                .fetch_optional(conn)
                .await?,
        );
    }
    let name = q.trim_start_matches('@');
    if name.is_empty() {
        return Ok(None);
    }
    Ok(sqlx::query_scalar!(
        "SELECT tg_user_id FROM users WHERE lower(username) = lower($1) ORDER BY last_seen_at DESC LIMIT 1",
        name
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn user_card(conn: &mut PgConnection, user_id: i64) -> Result<UserCard, StorageError> {
    let u = sqlx::query!(
        r#"
        SELECT tg_user_id, username, first_name, is_premium, daily_limit_usd, risk_note,
               created_at, last_seen_at, first_completed_at
          FROM users WHERE tg_user_id = $1
        "#,
        user_id
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(StorageError::NotFound(user_id))?;
    let s = sqlx::query!(
        r#"
        SELECT count(*) FILTER (WHERE state = 'COMPLETED')          AS "completed!",
               count(*) FILTER (WHERE state = 'REFUNDED')           AS "refunded!",
               count(*) FILTER (WHERE state = 'MANUAL_REVIEW')      AS "in_review!",
               count(*) FILTER (WHERE NOT is_terminal)              AS "live!",
               COALESCE(sum(intake_amount) FILTER (WHERE state = 'COMPLETED'), 0) AS "turnover!",
               COALESCE(sum(fee_amount) FILTER (WHERE state = 'COMPLETED'), 0)    AS "fees!"
          FROM orders WHERE user_id = $1
        "#,
        user_id
    )
    .fetch_one(&mut *conn)
    .await?;
    let ban = sqlx::query_as!(
        BanRow,
        r#"
        SELECT reason, banned_by, created_at, expires_at
          FROM bans
         WHERE user_id = $1 AND lifted_at IS NULL AND (expires_at IS NULL OR expires_at > now())
         ORDER BY created_at DESC LIMIT 1
        "#,
        user_id
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(UserCard {
        tg_user_id: u.tg_user_id,
        username: u.username,
        first_name: u.first_name,
        is_premium: u.is_premium,
        daily_limit_usd: u.daily_limit_usd,
        risk_note: u.risk_note,
        created_at: u.created_at,
        last_seen_at: u.last_seen_at,
        first_completed_at: u.first_completed_at,
        completed: s.completed,
        refunded: s.refunded,
        in_review: s.in_review,
        live: s.live,
        turnover: s.turnover,
        fees: s.fees,
        ban,
    })
}

/// Забанить: прежний истёкший бан закрывается, действующий — ошибка (сначала снять).
pub async fn ban(
    conn: &mut PgConnection,
    user_id: i64,
    reason: &str,
    expires_at: Option<DateTime<Utc>>,
    staff: i64,
) -> Result<(), StorageError> {
    sqlx::query!(
        r#"
        UPDATE bans SET lifted_at = expires_at, lift_reason = 'expired'
         WHERE user_id = $1 AND lifted_at IS NULL AND expires_at IS NOT NULL AND expires_at <= now()
        "#,
        user_id
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "INSERT INTO bans (user_id, reason, banned_by, expires_at) VALUES ($1, $2, $3, $4)",
        user_id,
        reason,
        staff,
        expires_at
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Снять бан; `false` — снимать нечего.
pub async fn unban(
    conn: &mut PgConnection,
    user_id: i64,
    reason: &str,
    staff: i64,
) -> Result<bool, StorageError> {
    let n = sqlx::query!(
        r#"
        UPDATE bans SET lifted_at = now(), lifted_by = $2, lift_reason = $3
         WHERE user_id = $1 AND lifted_at IS NULL
        "#,
        user_id,
        staff,
        reason
    )
    .execute(conn)
    .await?
    .rows_affected();
    Ok(n > 0)
}

/// Личный суточный лимит в долларах; `None` — общий.
pub async fn set_daily_limit(
    conn: &mut PgConnection,
    user_id: i64,
    limit: Option<Decimal>,
) -> Result<(), StorageError> {
    sqlx::query!(
        "UPDATE users SET daily_limit_usd = $2 WHERE tg_user_id = $1",
        user_id,
        limit
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn set_note(
    conn: &mut PgConnection,
    user_id: i64,
    note: Option<&str>,
) -> Result<(), StorageError> {
    sqlx::query!(
        "UPDATE users SET risk_note = $2 WHERE tg_user_id = $1",
        user_id,
        note
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Личный суточный лимит клиента (`users.daily_limit_usd`).
pub async fn daily_limit(
    conn: &mut PgConnection,
    user_id: i64,
) -> Result<Option<Decimal>, StorageError> {
    Ok(sqlx::query_scalar!(
        "SELECT daily_limit_usd FROM users WHERE tg_user_id = $1",
        user_id
    )
    .fetch_optional(conn)
    .await?
    .flatten())
}

// ---------------------------------------------------------------------------------------------
// Сводка

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateCount {
    pub state: String,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayStats {
    pub completed: i64,
    pub refunded: i64,
    pub turnover: Decimal,
    pub fees: Decimal,
}

/// Живые заявки по состояниям.
pub async fn live_by_state(conn: &mut PgConnection) -> Result<Vec<StateCount>, StorageError> {
    Ok(sqlx::query_as!(
        StateCount,
        r#"
        SELECT state, count(*) AS "count!"
          FROM orders WHERE NOT is_terminal GROUP BY state ORDER BY 2 DESC
        "#
    )
    .fetch_all(conn)
    .await?)
}

/// Итоги с момента `since`.
pub async fn stats_since(
    conn: &mut PgConnection,
    since: DateTime<Utc>,
) -> Result<DayStats, StorageError> {
    let r = sqlx::query!(
        r#"
        SELECT count(*) FILTER (WHERE state = 'COMPLETED') AS "completed!",
               count(*) FILTER (WHERE state = 'REFUNDED')  AS "refunded!",
               COALESCE(sum(intake_amount) FILTER (WHERE state = 'COMPLETED'), 0) AS "turnover!",
               COALESCE(sum(fee_amount) FILTER (WHERE state = 'COMPLETED'), 0)    AS "fees!"
          FROM orders WHERE finished_at >= $1
        "#,
        since
    )
    .fetch_one(conn)
    .await?;
    Ok(DayStats {
        completed: r.completed,
        refunded: r.refunded,
        turnover: r.turnover,
        fees: r.fees,
    })
}

/// День статистики (по часовому поясу владельца): завершённые обмены, оборот (пришло от
/// клиентов), валовая прибыль (комиссии).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayPoint {
    /// Начало дня или часа по местному времени.
    pub bucket: chrono::NaiveDateTime,
    pub completed: i64,
    pub turnover: Decimal,
    pub fees: Decimal,
}

/// Направление за период: сколько обменов и p95 времени заявки в секундах.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectionPoint {
    pub direction: String,
    pub completed: i64,
    pub p95_secs: i64,
}

/// Ручной разбор за период: заявок, медиана и максимум их полного времени, секунды.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManualPoint {
    pub orders: i64,
    pub median_secs: i64,
    pub max_secs: i64,
}

/// Статистика по дням или часам (`unit`: `day` / `hour`) за `[from, to)` в поясе `tz`
/// (IANA, например `Europe/Moscow`).
pub async fn daily_stats(
    conn: &mut PgConnection,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    tz: &str,
    unit: &str,
) -> Result<Vec<DayPoint>, StorageError> {
    Ok(sqlx::query_as!(
        DayPoint,
        r#"
        SELECT date_trunc($4, finished_at AT TIME ZONE $3) AS "bucket!",
               count(*) AS "completed!",
               COALESCE(sum(intake_amount), 0) AS "turnover!",
               COALESCE(sum(fee_amount), 0) AS "fees!"
          FROM orders
         WHERE state = 'COMPLETED' AND finished_at >= $1 AND finished_at < $2
         GROUP BY 1 ORDER BY 1
        "#,
        from,
        to,
        tz,
        unit
    )
    .fetch_all(conn)
    .await?)
}

/// Местные «сейчас» и полночь сегодня в поясе `tz` (UTC): границы периодов статистики.
pub async fn local_midnight(
    conn: &mut PgConnection,
    tz: &str,
) -> Result<(chrono::NaiveDateTime, DateTime<Utc>), StorageError> {
    let r = sqlx::query!(
        r#"
        SELECT (now() AT TIME ZONE $1) AS "local_now!",
               (date_trunc('day', now() AT TIME ZONE $1) AT TIME ZONE $1) AS "midnight!"
        "#,
        tz
    )
    .fetch_one(conn)
    .await?;
    Ok((r.local_now, r.midnight))
}

/// По направлениям за `[from, to)`: обменов и p95 времени заявки.
pub async fn direction_stats(
    conn: &mut PgConnection,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<DirectionPoint>, StorageError> {
    Ok(sqlx::query_as!(
        DirectionPoint,
        r#"
        SELECT direction AS "direction!",
               count(*) AS "completed!",
               COALESCE(percentile_cont(0.95) WITHIN GROUP (
                   ORDER BY extract(epoch FROM finished_at - created_at)), 0)::bigint AS "p95_secs!"
          FROM orders
         WHERE state = 'COMPLETED' AND finished_at >= $1 AND finished_at < $2
         GROUP BY direction ORDER BY 2 DESC
        "#,
        from,
        to
    )
    .fetch_all(conn)
    .await?)
}

/// Заявки, побывавшие на ручном разборе и закрытые за `[from, to)`.
pub async fn manual_stats(
    conn: &mut PgConnection,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<ManualPoint, StorageError> {
    Ok(sqlx::query_as!(
        ManualPoint,
        r#"
        SELECT count(*) AS "orders!",
               COALESCE(percentile_cont(0.5) WITHIN GROUP (
                   ORDER BY extract(epoch FROM o.finished_at - o.created_at)), 0)::bigint AS "median_secs!",
               COALESCE(max(extract(epoch FROM o.finished_at - o.created_at)), 0)::bigint AS "max_secs!"
          FROM orders o
         WHERE o.finished_at >= $1 AND o.finished_at < $2
           AND EXISTS (SELECT 1 FROM order_transitions t
                        WHERE t.order_id = o.id AND t.to_state = 'MANUAL_REVIEW')
        "#,
        from,
        to
    )
    .fetch_one(conn)
    .await?)
}

// ---------------------------------------------------------------------------------------------
// Пороги резерва

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds {
    pub target: Decimal,
    pub low: Decimal,
    pub critical: Decimal,
    pub high: Decimal,
}

impl Thresholds {
    /// Пороги из одной цели: критично — 25 %, мало — 50 %, излишек — 200 % цели.
    pub fn from_target(target: Decimal) -> Self {
        Self {
            target,
            low: target / Decimal::from(2),
            critical: target / Decimal::from(4),
            high: target * Decimal::from(2),
        }
    }
}

pub async fn thresholds(
    conn: &mut PgConnection,
    wallet_account_id: i16,
    asset: Asset,
) -> Result<Option<Thresholds>, StorageError> {
    Ok(sqlx::query_as!(
        Thresholds,
        r#"
        SELECT target, low, critical, high
          FROM reserve_thresholds WHERE wallet_account_id = $1 AND asset = $2
        "#,
        wallet_account_id,
        asset.code()
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn set_thresholds(
    conn: &mut PgConnection,
    wallet_account_id: i16,
    asset: Asset,
    t: Thresholds,
) -> Result<(), StorageError> {
    sqlx::query!(
        r#"
        INSERT INTO reserve_thresholds (wallet_account_id, asset, target, low, critical, high)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (wallet_account_id, asset) DO UPDATE
           SET target = EXCLUDED.target, low = EXCLUDED.low, critical = EXCLUDED.critical,
               high = EXCLUDED.high
        "#,
        wallet_account_id,
        asset.code(),
        t.target,
        t.low,
        t.critical,
        t.high
    )
    .execute(conn)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Юзерботы

/// Последняя сверка кошелька (без «пропущено: операции в полёте»).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastReconciliation {
    pub wallet_account_id: i16,
    pub expected: Decimal,
    pub actual: Decimal,
    /// `ok`, `warn`, `pause`.
    pub verdict: String,
    pub at: DateTime<Utc>,
}

/// Последняя сверка каждого кошелька по USDT (главный экран пульта: «сверка 14:00»,
/// «Леджер · Реально»).
pub async fn last_reconciliations(
    conn: &mut PgConnection,
) -> Result<Vec<LastReconciliation>, StorageError> {
    Ok(sqlx::query_as!(
        LastReconciliation,
        r#"
        SELECT DISTINCT ON (wallet_account_id)
               wallet_account_id, expected, actual, verdict, at
          FROM reconciliations
         WHERE asset = 'USDT' AND verdict <> 'skipped_inflight'
         ORDER BY wallet_account_id, at DESC
        "#
    )
    .fetch_all(conn)
    .await?)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserbotRow {
    pub id: i16,
    pub label: String,
    pub status: String,
    pub flood_wait_until: Option<DateTime<Utc>>,
    pub last_ok_at: Option<DateTime<Utc>>,
    pub in_flight: i64,
}

pub async fn userbots(conn: &mut PgConnection) -> Result<Vec<UserbotRow>, StorageError> {
    Ok(sqlx::query_as!(
        UserbotRow,
        r#"
        SELECT u.id, u.label, u.status, u.flood_wait_until, u.last_ok_at,
               (SELECT count(*) FROM operations op JOIN wallet_accounts w ON w.id = op.wallet_account_id
                 WHERE w.userbot_id = u.id AND op.status IN ('dispatched', 'unknown')) AS "in_flight!"
          FROM userbot_accounts u ORDER BY u.id
        "#
    )
    .fetch_all(conn)
    .await?)
}

/// Пауза аккаунта юзербота на время ручных действий (`disabled`) или снятие (`active`).
pub async fn set_userbot_paused(
    conn: &mut PgConnection,
    id: i16,
    paused: bool,
) -> Result<(), StorageError> {
    let status = if paused { "disabled" } else { "active" };
    sqlx::query!(
        "UPDATE userbot_accounts SET status = $2 WHERE id = $1",
        id,
        status
    )
    .execute(conn)
    .await?;
    Ok(())
}
