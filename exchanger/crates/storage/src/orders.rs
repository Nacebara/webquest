//! Заявки: создание и переходы через `domain::fsm` с оптимистичной блокировкой (SPEC §10.5.1).
//!
//! Эффекты перехода (операции outbox, удержания, проводки) возвращаются вызывающему коду:
//! он выполняет их в той же транзакции, что и переход.

use domain::fsm::{self, Event, OrderState, Transition};
use domain::{Asset, Decimal, Direction};
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
    /// `CQ…` / `mc_…` — для обмена чека. Клиент платит только чеком (v0.2).
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
    pub fee_pct_applied: Option<Decimal>,
    pub fee_amount: Option<Decimal>,
    pub payout_amount: Option<Decimal>,
    pub refund_amount: Option<Decimal>,
    /// Ссылка на чек выплаты или возврата, созданный юзерботом.
    pub payout_check_url: Option<&'a str>,
    pub failure_code: Option<&'a str>,
    pub manual_reason: Option<&'a str>,
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
               fee_pct_applied = COALESCE($6, fee_pct_applied),
               fee_amount      = COALESCE($7, fee_amount),
               payout_amount   = COALESCE($8, payout_amount),
               refund_amount   = COALESCE($9, refund_amount),
               failure_code    = COALESCE($10, failure_code),
               manual_reason   = COALESCE($11, manual_reason),
               payout_check_url = COALESCE($14, payout_check_url),
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
    )
    .fetch_optional(&mut *conn)
    .await?
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
    pub fee_pct_applied: Option<Decimal>,
    pub fee_amount: Option<Decimal>,
    pub payout_amount: Option<Decimal>,
    pub refund_amount: Option<Decimal>,
    pub invoice_amount: Option<Decimal>,
    pub payout_check_url: Option<String>,
    pub failure_code: Option<String>,
    pub manual_reason: Option<String>,
}

pub async fn details(conn: &mut PgConnection, id: i64) -> Result<OrderDetails, StorageError> {
    let r = sqlx::query!(
        r#"
        SELECT intake_start_param, invoice_start_param, intake_amount, fee_pct_applied,
               fee_amount, payout_amount, refund_amount, invoice_amount, payout_check_url,
               failure_code, manual_reason
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
        fee_pct_applied: r.fee_pct_applied,
        fee_amount: r.fee_amount,
        payout_amount: r.payout_amount,
        refund_amount: r.refund_amount,
        invoice_amount: r.invoice_amount,
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
