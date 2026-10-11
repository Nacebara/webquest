//! Запросы клиентского бота (`tgbot`): сообщение прогресса заявки, вид заявки для экрана,
//! история и карточка, активный бан, живая заявка по чеку, заявки для пересинхронизации.
//!
//! Переходы заявки здесь не делаются: только чтение и поля `progress_chat_id` /
//! `progress_message_id`. Учтите: триггер `orders_guard_transition` увеличивает `version`
//! и `updated_at` при любом UPDATE заявки, в том числе при записи сообщения прогресса.
//! Движок поэтому перечитывает заявку `FOR UPDATE` перед каждым переходом.

use chrono::{DateTime, Utc};
use domain::fsm::OrderState;
use domain::{Asset, Decimal, Direction, Platform};
use sqlx::PgConnection;

use crate::error::StorageError;

/// Сообщение бота, которое показывает ход заявки (одно на заявку, правится по шагам).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressMessage {
    pub chat_id: i64,
    pub message_id: i32,
}

/// Всё, что нужно для экрана заявки клиенту.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderView {
    pub id: i64,
    pub public_id: String,
    pub user_id: i64,
    pub direction: Direction,
    pub asset: Asset,
    pub state: OrderState,
    pub intake_amount: Option<Decimal>,
    pub fee_pct_applied: Option<Decimal>,
    pub fee_amount: Option<Decimal>,
    pub payout_amount: Option<Decimal>,
    pub refund_amount: Option<Decimal>,
    pub invoice_amount: Option<Decimal>,
    /// Оплата счёта: сколько прислать (`X`) и до какого времени держится цена.
    pub quote_amount_in: Option<Decimal>,
    pub quote_expires_at: Option<DateTime<Utc>>,
    /// Последний переход — «чек не принят» (оплата счёта ждёт другой чек).
    pub last_check_rejected: bool,
    /// Ссылка на чек выплаты или возврата.
    pub payout_check_url: Option<String>,
    pub failure_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Когда заявка вошла в текущее состояние (по журналу переходов).
    pub state_since: DateTime<Utc>,
    pub progress: Option<ProgressMessage>,
}

/// Строка истории клиента.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub public_id: String,
    pub created_at: DateTime<Utc>,
    pub direction: Direction,
    pub asset: Asset,
    /// Сколько пришло от клиента (`None` — чек не активирован).
    pub intake_amount: Option<Decimal>,
    pub state: OrderState,
}

/// Действующий бан клиента.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveBan {
    pub reason: String,
    /// `None` — бессрочно.
    pub expires_at: Option<DateTime<Utc>>,
}

/// Живая (не терминальная) заявка по этому чеку.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveCheckOrder {
    pub order_id: i64,
    pub public_id: String,
    pub user_id: i64,
}

fn parse<T: std::str::FromStr>(column: &'static str, value: &str) -> Result<T, StorageError> {
    value
        .parse()
        .map_err(|_| StorageError::corrupt(column, value))
}

fn progress(chat_id: Option<i64>, message_id: Option<i32>) -> Option<ProgressMessage> {
    match (chat_id, message_id) {
        (Some(chat_id), Some(message_id)) => Some(ProgressMessage {
            chat_id,
            message_id,
        }),
        _ => None,
    }
}

/// Сообщение прогресса заявки, если оно уже отправлено.
pub async fn progress_message(
    conn: &mut PgConnection,
    order_id: i64,
) -> Result<Option<ProgressMessage>, StorageError> {
    let row = sqlx::query!(
        "SELECT progress_chat_id, progress_message_id FROM orders WHERE id = $1",
        order_id
    )
    .fetch_optional(conn)
    .await?
    .ok_or(StorageError::NotFound(order_id))?;
    Ok(progress(row.progress_chat_id, row.progress_message_id))
}

/// Запомнить сообщение прогресса (после отправки или после замены удалённого клиентом).
pub async fn set_progress_message(
    conn: &mut PgConnection,
    order_id: i64,
    message: ProgressMessage,
) -> Result<(), StorageError> {
    let updated = sqlx::query!(
        "UPDATE orders SET progress_chat_id = $2, progress_message_id = $3 WHERE id = $1",
        order_id,
        message.chat_id,
        message.message_id
    )
    .execute(conn)
    .await?
    .rows_affected();
    if updated == 0 {
        return Err(StorageError::NotFound(order_id));
    }
    Ok(())
}

/// Это сообщение — сообщение прогресса какой-то заявки (его нельзя затирать другим экраном).
pub async fn is_progress_message(
    conn: &mut PgConnection,
    chat_id: i64,
    message_id: i32,
) -> Result<bool, StorageError> {
    let found = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM orders
                           WHERE progress_chat_id = $1 AND progress_message_id = $2) AS "found!""#,
        chat_id,
        message_id
    )
    .fetch_one(conn)
    .await?;
    Ok(found)
}

struct ViewRow {
    id: i64,
    public_id: String,
    user_id: i64,
    direction: String,
    asset: String,
    state: String,
    intake_amount: Option<Decimal>,
    fee_pct_applied: Option<Decimal>,
    fee_amount: Option<Decimal>,
    payout_amount: Option<Decimal>,
    refund_amount: Option<Decimal>,
    invoice_amount: Option<Decimal>,
    quote_amount_in: Option<Decimal>,
    quote_expires_at: Option<DateTime<Utc>>,
    last_check_rejected: bool,
    payout_check_url: Option<String>,
    failure_code: Option<String>,
    created_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    state_since: DateTime<Utc>,
    progress_chat_id: Option<i64>,
    progress_message_id: Option<i32>,
}

impl ViewRow {
    fn into_view(self) -> Result<OrderView, StorageError> {
        Ok(OrderView {
            id: self.id,
            public_id: self.public_id,
            user_id: self.user_id,
            direction: parse("orders.direction", &self.direction)?,
            asset: parse("orders.asset", &self.asset)?,
            state: parse("orders.state", &self.state)?,
            intake_amount: self.intake_amount,
            fee_pct_applied: self.fee_pct_applied,
            fee_amount: self.fee_amount,
            payout_amount: self.payout_amount,
            refund_amount: self.refund_amount,
            invoice_amount: self.invoice_amount,
            quote_amount_in: self.quote_amount_in,
            quote_expires_at: self.quote_expires_at,
            last_check_rejected: self.last_check_rejected,
            payout_check_url: self.payout_check_url,
            failure_code: self.failure_code,
            created_at: self.created_at,
            finished_at: self.finished_at,
            state_since: self.state_since,
            progress: progress(self.progress_chat_id, self.progress_message_id),
        })
    }
}

/// Заявка для экрана по внутреннему id.
pub async fn order_view(
    conn: &mut PgConnection,
    order_id: i64,
) -> Result<Option<OrderView>, StorageError> {
    let row = sqlx::query_as!(
        ViewRow,
        r#"
        SELECT o.id, o.public_id, o.user_id, o.direction, o.asset, o.state,
               o.intake_amount, o.fee_pct_applied, o.fee_amount, o.payout_amount,
               o.refund_amount, o.invoice_amount, o.quote_amount_in, o.quote_expires_at,
               COALESCE((SELECT t.data->>'event' = 'CheckRejected' FROM order_transitions t
                          WHERE t.order_id = o.id ORDER BY t.id DESC LIMIT 1),
                        FALSE) AS "last_check_rejected!",
               o.payout_check_url, o.failure_code, o.created_at, o.finished_at,
               COALESCE((SELECT max(t.at) FROM order_transitions t
                          WHERE t.order_id = o.id AND t.to_state = o.state),
                        o.created_at) AS "state_since!",
               o.progress_chat_id, o.progress_message_id
          FROM orders o
         WHERE o.id = $1
        "#,
        order_id
    )
    .fetch_optional(conn)
    .await?;
    row.map(ViewRow::into_view).transpose()
}

/// Заявка клиента по публичному номеру. Чужую заявку не отдаёт.
pub async fn order_view_for_user(
    conn: &mut PgConnection,
    user_id: i64,
    public_id: &str,
) -> Result<Option<OrderView>, StorageError> {
    let row = sqlx::query_as!(
        ViewRow,
        r#"
        SELECT o.id, o.public_id, o.user_id, o.direction, o.asset, o.state,
               o.intake_amount, o.fee_pct_applied, o.fee_amount, o.payout_amount,
               o.refund_amount, o.invoice_amount, o.quote_amount_in, o.quote_expires_at,
               COALESCE((SELECT t.data->>'event' = 'CheckRejected' FROM order_transitions t
                          WHERE t.order_id = o.id ORDER BY t.id DESC LIMIT 1),
                        FALSE) AS "last_check_rejected!",
               o.payout_check_url, o.failure_code, o.created_at, o.finished_at,
               COALESCE((SELECT max(t.at) FROM order_transitions t
                          WHERE t.order_id = o.id AND t.to_state = o.state),
                        o.created_at) AS "state_since!",
               o.progress_chat_id, o.progress_message_id
          FROM orders o
         WHERE o.public_id = $1 AND o.user_id = $2
        "#,
        public_id,
        user_id
    )
    .fetch_optional(conn)
    .await?;
    row.map(ViewRow::into_view).transpose()
}

/// История клиента, новые сверху: `limit` строк начиная с `offset`.
pub async fn history(
    conn: &mut PgConnection,
    user_id: i64,
    limit: i64,
    offset: i64,
) -> Result<Vec<HistoryEntry>, StorageError> {
    let rows = sqlx::query!(
        r#"
        SELECT public_id, created_at, direction, asset, intake_amount, state
          FROM orders
         WHERE user_id = $1
         ORDER BY created_at DESC, id DESC
         LIMIT $2 OFFSET $3
        "#,
        user_id,
        limit,
        offset
    )
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|r| {
            Ok(HistoryEntry {
                public_id: r.public_id,
                created_at: r.created_at,
                direction: parse("orders.direction", &r.direction)?,
                asset: parse("orders.asset", &r.asset)?,
                intake_amount: r.intake_amount,
                state: parse("orders.state", &r.state)?,
            })
        })
        .collect()
}

/// Действующий бан: не снят и не истёк.
pub async fn active_ban(
    conn: &mut PgConnection,
    user_id: i64,
) -> Result<Option<ActiveBan>, StorageError> {
    let row = sqlx::query!(
        r#"
        SELECT reason, expires_at
          FROM bans
         WHERE user_id = $1 AND lifted_at IS NULL AND (expires_at IS NULL OR expires_at > now())
         ORDER BY created_at DESC
         LIMIT 1
        "#,
        user_id
    )
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|r| ActiveBan {
        reason: r.reason,
        expires_at: r.expires_at,
    }))
}

/// Живая заявка, в которой уже обрабатывается этот чек (индекс `orders_intake_param_live`).
pub async fn live_order_for_check(
    conn: &mut PgConnection,
    platform: Platform,
    start_param: &str,
) -> Result<Option<LiveCheckOrder>, StorageError> {
    let row = sqlx::query!(
        r#"
        SELECT id, public_id, user_id
          FROM orders
         WHERE intake_platform = $1 AND intake_start_param = $2 AND NOT is_terminal
        "#,
        platform.db_name(),
        start_param
    )
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|r| LiveCheckOrder {
        order_id: r.id,
        public_id: r.public_id,
        user_id: r.user_id,
    }))
}

/// Заявки, сообщение которых надо сверить с текущим состоянием: живые и закончившиеся после
/// `since`. Отказы при приёме (`REJECTED`) без сообщения не берутся: клиенту уже ответили
/// экраном ошибки. `missing_only` — только заявки, сообщение которых ещё не отправлено.
pub async fn orders_to_resync(
    conn: &mut PgConnection,
    since: DateTime<Utc>,
    missing_only: bool,
    limit: i64,
) -> Result<Vec<i64>, StorageError> {
    let ids = sqlx::query_scalar!(
        r#"
        SELECT id
          FROM orders
         WHERE (NOT is_terminal OR finished_at >= $1)
           AND (progress_message_id IS NOT NULL OR state <> 'REJECTED')
           AND (NOT $2 OR progress_message_id IS NULL)
         ORDER BY id
         LIMIT $3
        "#,
        since,
        missing_only,
        limit
    )
    .fetch_all(conn)
    .await?;
    Ok(ids)
}
