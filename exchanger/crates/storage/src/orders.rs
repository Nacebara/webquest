//! Заявки: создание и переходы через `domain::fsm` с оптимистичной блокировкой (SPEC §10.5.1).
//!
//! Эффекты перехода (операции outbox, удержания, проводки) возвращаются вызывающему коду:
//! он выполняет их в той же транзакции, что и переход.

use chrono::{DateTime, Utc};
use domain::fsm::{self, Event, OrderState, Transition};
use domain::{Asset, Decimal, Direction, Platform};
use sqlx::PgConnection;

use crate::error::{StorageError, unique_violation};

/// Кто сделал переход (`order_transitions.actor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    System,
    User,
    Staff(i64),
}

impl Actor {
    fn as_db(self) -> String {
        match self {
            Actor::System => "system".to_owned(),
            Actor::User => "user".to_owned(),
            Actor::Staff(id) => format!("staff:{id}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewOrder<'a> {
    pub user_id: i64,
    pub direction: Direction,
    pub asset: Asset,
    /// `CQ…` / `t_…` — для обмена чека. Клиент платит только чеком (v0.2).
    pub intake_start_param: Option<&'a str>,
    /// `IV…` / `inv_…` — для оплаты чужого счёта.
    pub invoice_start_param: Option<&'a str>,
}

/// Минимум полей заявки, нужный для перехода.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderRow {
    pub id: i64,
    pub public_id: String,
    pub user_id: i64,
    pub direction: Direction,
    pub asset: Asset,
    pub state: OrderState,
    pub version: i32,
}

/// Поля, которые записываются тем же UPDATE, что и переход (атомарно с ним).
/// `None` — не трогать.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrderPatch<'a> {
    pub intake_amount: Option<Decimal>,
    /// Актив, в котором реально пришли деньги.
    pub intake_asset: Option<Asset>,
    pub fee_pct_applied: Option<Decimal>,
    pub fee_amount: Option<Decimal>,
    pub payout_amount: Option<Decimal>,
    pub refund_amount: Option<Decimal>,
    /// Ссылка на чек выплаты или возврата, созданный юзерботом.
    pub payout_check_url: Option<&'a str>,
    pub failure_code: Option<&'a str>,
    pub manual_reason: Option<&'a str>,
    /// Чек клиента для оплаты счёта: приходит после котировки, каждый следующий (при
    /// недоплате) заменяет предыдущий — тот уже зачислен.
    pub intake_start_param: Option<&'a str>,
    /// Сумма счёта в активе (`A`).
    pub invoice_amount: Option<Decimal>,
    /// Сколько клиент должен прислать (`X`).
    pub quote_amount_in: Option<Decimal>,
    pub quote_expires_at: Option<DateTime<Utc>>,
}

/// Создать заявку в состоянии `NEW` и записать первый переход в журнал.
pub async fn insert(conn: &mut PgConnection, new: &NewOrder<'_>) -> Result<OrderRow, StorageError> {
    let source = new.direction.source();
    let invoice_platform = new
        .invoice_start_param
        .map(|_| new.direction.target().db_name());
    let row = sqlx::query!(
        r#"
        INSERT INTO orders (user_id, direction, asset, intake_method, intake_platform,
                            intake_start_param, invoice_platform, invoice_start_param)
        VALUES ($1, $2, $3, 'check', $4, $5, $6, $7)
        RETURNING id, public_id, state, version
        "#,
        new.user_id,
        new.direction.code(),
        new.asset.code(),
        source.db_name(),
        new.intake_start_param,
        invoice_platform,
        new.invoice_start_param,
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(map_duplicate)?;

    sqlx::query!(
        r#"INSERT INTO order_transitions (order_id, from_state, to_state, actor) VALUES ($1, NULL, $2, 'system')"#,
        row.id,
        row.state,
    )
    .execute(&mut *conn)
    .await?;

    Ok(OrderRow {
        id: row.id,
        public_id: row.public_id,
        user_id: new.user_id,
        direction: new.direction,
        asset: new.asset,
        state: parse_state(&row.state)?,
        version: row.version,
    })
}

/// Прочитать заявку; `for_update` — со строковой блокировкой до конца транзакции.
pub async fn get(
    conn: &mut PgConnection,
    id: i64,
    for_update: bool,
) -> Result<OrderRow, StorageError> {
    let row = if for_update {
        sqlx::query!(
            r#"SELECT id, public_id, user_id, direction, asset, state, version FROM orders WHERE id = $1 FOR UPDATE"#,
            id
        )
        .fetch_optional(&mut *conn)
        .await?
        .map(|r| (r.id, r.public_id, r.user_id, r.direction, r.asset, r.state, r.version))
    } else {
        sqlx::query!(
            r#"SELECT id, public_id, user_id, direction, asset, state, version FROM orders WHERE id = $1"#,
            id
        )
        .fetch_optional(&mut *conn)
        .await?
        .map(|r| (r.id, r.public_id, r.user_id, r.direction, r.asset, r.state, r.version))
    };
    let (id, public_id, user_id, direction, asset, state, version) =
        row.ok_or(StorageError::NotFound(id))?;
    Ok(OrderRow {
        id,
        public_id,
        user_id,
        direction: direction
            .parse()
            .map_err(|_| StorageError::corrupt("orders.direction", &direction))?,
        asset: asset
            .parse()
            .map_err(|_| StorageError::corrupt("orders.asset", &asset))?,
        state: parse_state(&state)?,
        version,
    })
}

/// Применить событие: проверка FSM → UPDATE с проверкой версии и состояния → запись в журнал.
/// Возвращает обновлённую заявку и эффекты, которые нужно выполнить в этой же транзакции.
pub async fn apply_event(
    conn: &mut PgConnection,
    order: &OrderRow,
    event: Event,
    patch: &OrderPatch<'_>,
    actor: Actor,
    reason: Option<&str>,
) -> Result<(OrderRow, Transition), StorageError> {
    let transition = fsm::transition(order.direction.flow(), order.state, event)?;
    let to = transition.to;
    let mark_received = to == OrderState::Received;

    let new_version = sqlx::query_scalar!(
        r#"
        UPDATE orders
           SET state           = $3,
               intake_amount   = COALESCE($5, intake_amount),
               intake_asset    = COALESCE($15, intake_asset),
               fee_pct_applied = COALESCE($6, fee_pct_applied),
               fee_amount      = COALESCE($7, fee_amount),
               payout_amount   = COALESCE($8, payout_amount),
               refund_amount   = COALESCE($9, refund_amount),
               failure_code    = COALESCE($10, failure_code),
               manual_reason   = COALESCE($11, manual_reason),
               payout_check_url = COALESCE($14, payout_check_url),
               intake_start_param = COALESCE($16, intake_start_param),
               invoice_amount  = COALESCE($17, invoice_amount),
               quote_amount_in = COALESCE($18, quote_amount_in),
               quote_expires_at = COALESCE($19, quote_expires_at),
               received_at     = CASE WHEN $12 AND received_at IS NULL THEN now() ELSE received_at END,
               finished_at     = CASE WHEN $4 THEN now() ELSE finished_at END
         WHERE id = $1 AND version = $2 AND state = $13
        RETURNING version
        "#,
        order.id,
        order.version,
        to.as_db_str(),
        to.is_terminal(),
        patch.intake_amount,
        patch.fee_pct_applied,
        patch.fee_amount,
        patch.payout_amount,
        patch.refund_amount,
        patch.failure_code,
        patch.manual_reason,
        mark_received,
        order.state.as_db_str(),
        patch.payout_check_url,
        patch.intake_asset.map(|a| a.code()),
        patch.intake_start_param,
        patch.invoice_amount,
        patch.quote_amount_in,
        patch.quote_expires_at,
    )
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_duplicate)?
    .ok_or(StorageError::Conflict {
        id: order.id,
        expected: order.version,
    })?;

    let data = serde_json::json!({ "event": event });
    sqlx::query!(
        r#"
        INSERT INTO order_transitions (order_id, from_state, to_state, actor, reason, data)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
        order.id,
        order.state.as_db_str(),
        to.as_db_str(),
        actor.as_db(),
        reason,
        data,
    )
    .execute(&mut *conn)
    .await?;

    let updated = OrderRow {
        state: to,
        version: new_version,
        ..order.clone()
    };
    Ok((updated, transition))
}

/// Суммы и параметры заявки, нужные для эффектов перехода и для экрана клиента.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrderDetails {
    pub intake_start_param: Option<String>,
    pub invoice_start_param: Option<String>,
    pub intake_amount: Option<Decimal>,
    pub intake_asset: Option<Asset>,
    pub fee_pct_applied: Option<Decimal>,
    pub fee_amount: Option<Decimal>,
    pub payout_amount: Option<Decimal>,
    pub refund_amount: Option<Decimal>,
    pub invoice_amount: Option<Decimal>,
    pub quote_amount_in: Option<Decimal>,
    pub quote_expires_at: Option<DateTime<Utc>>,
    pub payout_check_url: Option<String>,
    pub failure_code: Option<String>,
    pub manual_reason: Option<String>,
}

pub async fn details(conn: &mut PgConnection, id: i64) -> Result<OrderDetails, StorageError> {
    let r = sqlx::query!(
        r#"
        SELECT intake_start_param, invoice_start_param, intake_amount, intake_asset, fee_pct_applied,
               fee_amount, payout_amount, refund_amount, invoice_amount, quote_amount_in,
               quote_expires_at, payout_check_url, failure_code, manual_reason
          FROM orders WHERE id = $1
        "#,
        id
    )
    .fetch_optional(conn)
    .await?
    .ok_or(StorageError::NotFound(id))?;
    Ok(OrderDetails {
        intake_start_param: r.intake_start_param,
        invoice_start_param: r.invoice_start_param,
        intake_amount: r.intake_amount,
        intake_asset: r
            .intake_asset
            .map(|a| {
                a.parse()
                    .map_err(|_| StorageError::corrupt("orders.intake_asset", &a))
            })
            .transpose()?,
        fee_pct_applied: r.fee_pct_applied,
        fee_amount: r.fee_amount,
        payout_amount: r.payout_amount,
        refund_amount: r.refund_amount,
        invoice_amount: r.invoice_amount,
        quote_amount_in: r.quote_amount_in,
        quote_expires_at: r.quote_expires_at,
        payout_check_url: r.payout_check_url,
        failure_code: r.failure_code,
        manual_reason: r.manual_reason,
    })
}

/// Клиент ещё не завершил ни одной заявки (для него — `new_user_max_amount`).
pub async fn is_new_user(conn: &mut PgConnection, user_id: i64) -> Result<bool, StorageError> {
    let done = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM orders WHERE user_id = $1 AND state = 'COMPLETED') AS "done!""#,
        user_id
    )
    .fetch_one(conn)
    .await?;
    Ok(!done)
}

/// Заявка клиента на оплату счёта, которая ждёт чек (`AWAITING_FUNDS`). `source` — платформа
/// чека клиента; `None` — любая. Самая ранняя, если их несколько.
pub async fn awaiting_funds(
    conn: &mut PgConnection,
    user_id: i64,
    source: Option<Platform>,
) -> Result<Option<OrderRow>, StorageError> {
    let id = sqlx::query_scalar!(
        r#"
        SELECT id FROM orders
         WHERE user_id = $1 AND state = 'AWAITING_FUNDS'
           AND ($2::TEXT IS NULL OR intake_platform = $2)
         ORDER BY id LIMIT 1
        "#,
        user_id,
        source.map(Platform::db_name),
    )
    .fetch_optional(&mut *conn)
    .await?;
    match id {
        Some(id) => Ok(Some(get(conn, id, false).await?)),
        None => Ok(None),
    }
}

/// Живая или успешно закрытая заявка по чужому счёту (повторная ссылка на счёт).
pub async fn by_invoice(
    conn: &mut PgConnection,
    platform: Platform,
    param: &str,
) -> Result<Option<OrderRow>, StorageError> {
    let id = sqlx::query_scalar!(
        r#"
        SELECT id FROM orders
         WHERE invoice_platform = $1 AND invoice_start_param = $2
           AND (NOT is_terminal OR state = 'COMPLETED')
         ORDER BY id DESC LIMIT 1
        "#,
        platform.db_name(),
        param,
    )
    .fetch_optional(&mut *conn)
    .await?;
    match id {
        Some(id) => Ok(Some(get(conn, id, false).await?)),
        None => Ok(None),
    }
}

/// Заявка клиента на оплату счёта, по которой сейчас проверяется чек платформы `source`
/// (`INTAKE_PENDING` / `INTAKE_UNKNOWN`).
pub async fn invoice_checking(
    conn: &mut PgConnection,
    user_id: i64,
    source: Platform,
) -> Result<Option<i64>, StorageError> {
    Ok(sqlx::query_scalar!(
        r#"
        SELECT id FROM orders
         WHERE user_id = $1 AND direction IN ('pay_cb_invoice', 'pay_xr_invoice')
           AND intake_platform = $2
           AND state IN ('INTAKE_PENDING', 'INTAKE_UNKNOWN')
         ORDER BY id LIMIT 1
        "#,
        user_id,
        source.db_name(),
    )
    .fetch_optional(conn)
    .await?)
}

/// Заявки, котировка которых истекла, пока они ждали чек.
pub async fn expired_quotes(conn: &mut PgConnection) -> Result<Vec<i64>, StorageError> {
    Ok(sqlx::query_scalar!(
        r#"
        SELECT id FROM orders
         WHERE state = 'AWAITING_FUNDS' AND quote_expires_at <= now()
         ORDER BY id
        "#
    )
    .fetch_all(conn)
    .await?)
}

/// Ссылка на чек, созданный после завершения заявки (переплата по оплаченному счёту).
pub async fn set_check_url(
    conn: &mut PgConnection,
    id: i64,
    url: &str,
) -> Result<(), StorageError> {
    sqlx::query!(
        "UPDATE orders SET payout_check_url = $2 WHERE id = $1",
        id,
        url
    )
    .execute(conn)
    .await?;
    Ok(())
}

fn parse_state(s: &str) -> Result<OrderState, StorageError> {
    s.parse()
        .map_err(|_| StorageError::corrupt("orders.state", s))
}

fn map_duplicate(err: sqlx::Error) -> StorageError {
    match unique_violation(&err) {
        Some("orders_intake_param_live") => StorageError::DuplicateLiveCheck,
        Some("orders_invoice_param_live" | "orders_invoice_param_completed") => {
            StorageError::DuplicateInvoice
        }
        _ => StorageError::Db(err),
    }
}

/// Объём клиента за последние 24 часа: пришедшие суммы и удержания ещё не оплаченных заявок
/// (без отказов). Для лимитов `daily_limit_usd` и «новый клиент» (SPEC §3.4).
pub async fn user_volume_24h(
    conn: &mut PgConnection,
    user_id: i64,
) -> Result<Decimal, StorageError> {
    let v = sqlx::query_scalar!(
        r#"
        SELECT COALESCE(SUM(COALESCE(o.intake_amount, h.amount, 0)), 0) AS "v!"
          FROM orders o
          LEFT JOIN holds h ON h.order_id = o.id AND h.state = 'active'
         WHERE o.user_id = $1
           AND o.created_at > now() - interval '24 hours'
           AND o.state NOT IN ('REJECTED', 'INTAKE_FAILED', 'EXPIRED', 'CANCELLED')
        "#,
        user_id
    )
    .fetch_one(conn)
    .await?;
    Ok(v)
}
