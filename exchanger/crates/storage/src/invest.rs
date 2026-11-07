//! Инвесторы (DESIGN-v0.2 Р18): доступ по приглашению владельца, пополнения и выводы.
//!
//! Деньги инвестора — долг сервиса перед ним в леджере (`liability:investor:<id>:USDT`):
//! баланс = пополнения + начисленная прибыль − выводы. Эта таблица — заявки и их статусы,
//! суммы истины — в леджере.

use chrono::{DateTime, Utc};
use domain::{Decimal, Platform};
use sqlx::PgConnection;

use crate::error::{StorageError, unique_violation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Investor {
    pub user_id: i64,
    /// Доля прибыли на его капитал, %.
    pub share_pct: Decimal,
    /// Пополнение можно вывести не раньше, чем через столько дней.
    pub lock_days: i32,
    pub min_deposit: Decimal,
    /// Вывод до этой суммы исполняется без владельца; 0 — только вручную.
    pub auto_withdraw_max: Decimal,
    pub is_active: bool,
    pub granted_by: Option<i64>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveKind {
    Deposit,
    Withdrawal,
}

impl MoveKind {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            MoveKind::Deposit => "deposit",
            MoveKind::Withdrawal => "withdrawal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveStatus {
    /// Вывод ждёт решения владельца.
    Requested,
    /// Операция юзербота поставлена или идёт.
    Processing,
    Done,
    Rejected,
    Failed,
    /// Неясный исход — решает человек.
    Manual,
}

impl MoveStatus {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            MoveStatus::Requested => "requested",
            MoveStatus::Processing => "processing",
            MoveStatus::Done => "done",
            MoveStatus::Rejected => "rejected",
            MoveStatus::Failed => "failed",
            MoveStatus::Manual => "manual",
        }
    }

    fn parse(s: &str) -> Result<Self, StorageError> {
        [
            MoveStatus::Requested,
            MoveStatus::Processing,
            MoveStatus::Done,
            MoveStatus::Rejected,
            MoveStatus::Failed,
            MoveStatus::Manual,
        ]
        .into_iter()
        .find(|m| m.as_db_str() == s)
        .ok_or_else(|| StorageError::corrupt("invest_moves.status", s))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub id: i64,
    pub user_id: i64,
    pub kind: MoveKind,
    pub status: MoveStatus,
    pub platform: Platform,
    pub amount: Option<Decimal>,
    pub start_param: Option<String>,
    pub check_url: Option<String>,
    pub note: Option<String>,
    pub decided_by: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

struct RawMove {
    id: i64,
    user_id: i64,
    kind: String,
    status: String,
    platform: String,
    amount: Option<Decimal>,
    start_param: Option<String>,
    check_url: Option<String>,
    note: Option<String>,
    decided_by: Option<i64>,
    created_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
}

impl TryFrom<RawMove> for Move {
    type Error = StorageError;

    fn try_from(r: RawMove) -> Result<Self, StorageError> {
        Ok(Self {
            id: r.id,
            user_id: r.user_id,
            kind: match r.kind.as_str() {
                "deposit" => MoveKind::Deposit,
                "withdrawal" => MoveKind::Withdrawal,
                other => return Err(StorageError::corrupt("invest_moves.kind", other)),
            },
            status: MoveStatus::parse(&r.status)?,
            platform: Platform::from_db_name(&r.platform)
                .ok_or_else(|| StorageError::corrupt("invest_moves.platform", &r.platform))?,
            amount: r.amount,
            start_param: r.start_param,
            check_url: r.check_url,
            note: r.note,
            decided_by: r.decided_by,
            created_at: r.created_at,
            finished_at: r.finished_at,
        })
    }
}

pub async fn investor(
    conn: &mut PgConnection,
    user_id: i64,
) -> Result<Option<Investor>, StorageError> {
    Ok(sqlx::query_as!(
        Investor,
        r#"
        SELECT user_id, share_pct, lock_days, min_deposit, auto_withdraw_max, is_active,
               granted_by, created_at
          FROM investors WHERE user_id = $1
        "#,
        user_id
    )
    .fetch_optional(conn)
    .await?)
}

/// Действующий инвестор (доступ не отключён).
pub async fn active(
    conn: &mut PgConnection,
    user_id: i64,
) -> Result<Option<Investor>, StorageError> {
    Ok(investor(conn, user_id).await?.filter(|i| i.is_active))
}

pub async fn list(conn: &mut PgConnection) -> Result<Vec<Investor>, StorageError> {
    Ok(sqlx::query_as!(
        Investor,
        r#"
        SELECT user_id, share_pct, lock_days, min_deposit, auto_withdraw_max, is_active,
               granted_by, created_at
          FROM investors ORDER BY created_at
        "#
    )
    .fetch_all(conn)
    .await?)
}

/// Выдать доступ или поменять условия.
pub async fn save(conn: &mut PgConnection, i: &Investor, staff: i64) -> Result<(), StorageError> {
    sqlx::query!(
        r#"
        INSERT INTO investors (user_id, share_pct, lock_days, min_deposit, auto_withdraw_max,
                               is_active, granted_by)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT (user_id) DO UPDATE
           SET share_pct = EXCLUDED.share_pct, lock_days = EXCLUDED.lock_days,
               min_deposit = EXCLUDED.min_deposit, auto_withdraw_max = EXCLUDED.auto_withdraw_max,
               is_active = EXCLUDED.is_active, updated_at = now()
        "#,
        i.user_id,
        i.share_pct,
        i.lock_days,
        i.min_deposit,
        i.auto_withdraw_max,
        i.is_active,
        staff,
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn insert_deposit(
    conn: &mut PgConnection,
    user_id: i64,
    platform: Platform,
    start_param: &str,
) -> Result<i64, StorageError> {
    Ok(sqlx::query_scalar!(
        r#"
        INSERT INTO invest_moves (user_id, kind, status, platform, start_param)
        VALUES ($1, 'deposit', 'processing', $2, $3)
        RETURNING id
        "#,
        user_id,
        platform.db_name(),
        start_param
    )
    .fetch_one(conn)
    .await?)
}

pub async fn insert_withdrawal(
    conn: &mut PgConnection,
    user_id: i64,
    platform: Platform,
    amount: Decimal,
) -> Result<i64, StorageError> {
    sqlx::query_scalar!(
        r#"
        INSERT INTO invest_moves (user_id, kind, status, platform, amount)
        VALUES ($1, 'withdrawal', 'requested', $2, $3)
        RETURNING id
        "#,
        user_id,
        platform.db_name(),
        amount
    )
    .fetch_one(conn)
    .await
    .map_err(|e| match unique_violation(&e) {
        Some("invest_moves_one_open_withdrawal") => StorageError::OpenWithdrawal,
        _ => StorageError::Db(e),
    })
}

pub async fn get(conn: &mut PgConnection, id: i64, for_update: bool) -> Result<Move, StorageError> {
    let row = if for_update {
        sqlx::query_as!(
            RawMove,
            r#"
            SELECT id, user_id, kind, status, platform, amount, start_param, check_url, note,
                   decided_by, created_at, finished_at
              FROM invest_moves WHERE id = $1 FOR UPDATE
            "#,
            id
        )
        .fetch_optional(conn)
        .await?
    } else {
        sqlx::query_as!(
            RawMove,
            r#"
            SELECT id, user_id, kind, status, platform, amount, start_param, check_url, note,
                   decided_by, created_at, finished_at
              FROM invest_moves WHERE id = $1
            "#,
            id
        )
        .fetch_optional(conn)
        .await?
    };
    row.ok_or(StorageError::NotFound(id))?.try_into()
}

/// Последние движения инвестора, новые сверху.
pub async fn recent(
    conn: &mut PgConnection,
    user_id: i64,
    limit: i64,
) -> Result<Vec<Move>, StorageError> {
    let rows = sqlx::query_as!(
        RawMove,
        r#"
        SELECT id, user_id, kind, status, platform, amount, start_param, check_url, note,
               decided_by, created_at, finished_at
          FROM invest_moves WHERE user_id = $1 ORDER BY id DESC LIMIT $2
        "#,
        user_id,
        limit
    )
    .fetch_all(conn)
    .await?;
    rows.into_iter().map(TryInto::try_into).collect()
}

/// Новый статус движения. `amount` / `check_url` / `note` — если заданы.
#[allow(clippy::too_many_arguments)]
pub async fn set_status(
    conn: &mut PgConnection,
    id: i64,
    status: MoveStatus,
    amount: Option<Decimal>,
    check_url: Option<&str>,
    note: Option<&str>,
    decided_by: Option<i64>,
) -> Result<(), StorageError> {
    let finished = matches!(
        status,
        MoveStatus::Done | MoveStatus::Rejected | MoveStatus::Failed
    );
    sqlx::query!(
        r#"
        UPDATE invest_moves
           SET status = $2, amount = COALESCE($3, amount), check_url = COALESCE($4, check_url),
               note = COALESCE($5, note), decided_by = COALESCE($6, decided_by),
               finished_at = CASE WHEN $7 THEN now() ELSE finished_at END
         WHERE id = $1
        "#,
        id,
        status.as_db_str(),
        amount,
        check_url,
        note,
        decided_by,
        finished,
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Цифры инвестора для экранов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Figures {
    /// Сколько его денег в обороте сейчас (баланс счёта в леджере).
    pub balance: Decimal,
    /// Начислено прибыли за всё время.
    pub profit: Decimal,
    /// Пополнения моложе срока блокировки — их пока нельзя вывести.
    pub locked: Decimal,
    /// Прибыль за последние 24 часа.
    pub profit_24h: Decimal,
}

impl Figures {
    pub fn withdrawable(&self) -> Decimal {
        (self.balance - self.locked).max(Decimal::ZERO)
    }
}

pub async fn figures(
    conn: &mut PgConnection,
    user_id: i64,
    lock_days: i32,
) -> Result<Figures, StorageError> {
    let code = domain::ledger::AccountCode::investor(user_id, domain::Asset::Usdt);
    let r = sqlx::query!(
        r#"
        SELECT COALESCE(-sum(e.amount), 0) AS "balance!",
               COALESCE(-sum(e.amount) FILTER (WHERE t.kind = 'invest_accrual'), 0) AS "profit!",
               COALESCE(-sum(e.amount) FILTER (WHERE t.kind = 'invest_accrual'
                                                AND t.created_at > now() - interval '24 hours'), 0)
                   AS "profit_24h!"
          FROM ledger_entries e
          JOIN ledger_accounts a ON a.id = e.account_id
          JOIN ledger_transactions t ON t.id = e.tx_id
         WHERE a.code = $1
        "#,
        code.as_str()
    )
    .fetch_one(&mut *conn)
    .await?;
    let locked = sqlx::query_scalar!(
        r#"
        SELECT COALESCE(sum(amount), 0) AS "locked!"
          FROM invest_moves
         WHERE user_id = $1 AND kind = 'deposit' AND status = 'done'
           AND finished_at > now() - make_interval(days => $2)
        "#,
        user_id,
        lock_days
    )
    .fetch_one(conn)
    .await?;
    Ok(Figures {
        balance: r.balance,
        profit: r.profit,
        locked,
        profit_24h: r.profit_24h,
    })
}

/// Всё по инвесторам для сводки пульта.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Totals {
    pub investors: i64,
    /// Сумма их денег в обороте.
    pub balance: Decimal,
    /// Начислено за 24 часа.
    pub accrued_24h: Decimal,
    /// Заявок на вывод ждут решения.
    pub requested: i64,
    /// Движений у человека (неясный исход).
    pub manual: i64,
}

pub async fn totals(conn: &mut PgConnection) -> Result<Totals, StorageError> {
    let r = sqlx::query!(
        r#"
        SELECT (SELECT count(*) FROM investors WHERE is_active) AS "investors!",
               (SELECT COALESCE(-sum(e.amount), 0)
                  FROM ledger_entries e JOIN ledger_accounts a ON a.id = e.account_id
                 WHERE a.code LIKE 'liability:investor:%') AS "balance!",
               (SELECT COALESCE(-sum(e.amount), 0)
                  FROM ledger_entries e
                  JOIN ledger_accounts a ON a.id = e.account_id
                  JOIN ledger_transactions t ON t.id = e.tx_id
                 WHERE a.code LIKE 'liability:investor:%' AND t.kind = 'invest_accrual'
                   AND t.created_at > now() - interval '24 hours') AS "accrued_24h!",
               (SELECT count(*) FROM invest_moves WHERE status = 'requested') AS "requested!",
               (SELECT count(*) FROM invest_moves WHERE status = 'manual') AS "manual!"
        "#
    )
    .fetch_one(conn)
    .await?;
    Ok(Totals {
        investors: r.investors,
        balance: r.balance,
        accrued_24h: r.accrued_24h,
        requested: r.requested,
        manual: r.manual,
    })
}

/// Движения, которые ждут человека: заявки на вывод и неясные исходы, старые сверху.
pub async fn awaiting_staff(
    conn: &mut PgConnection,
    limit: i64,
) -> Result<Vec<Move>, StorageError> {
    let rows = sqlx::query_as!(
        RawMove,
        r#"
        SELECT id, user_id, kind, status, platform, amount, start_param, check_url, note,
               decided_by, created_at, finished_at
          FROM invest_moves WHERE status IN ('requested', 'manual') ORDER BY id LIMIT $1
        "#,
        limit
    )
    .fetch_all(conn)
    .await?;
    rows.into_iter().map(TryInto::try_into).collect()
}

/// Последняя проводка леджера (курсор начисления прибыли).
pub async fn last_tx_id(conn: &mut PgConnection) -> Result<i64, StorageError> {
    Ok(
        sqlx::query_scalar!(r#"SELECT COALESCE(max(id), 0) AS "id!" FROM ledger_transactions"#)
            .fetch_one(conn)
            .await?,
    )
}

/// Кредитовый оборот счёта в проводках `(after, upto]` (выручка: кредит — минус в строках).
pub async fn credit_turnover(
    conn: &mut PgConnection,
    code: &str,
    after: i64,
    upto: i64,
) -> Result<Decimal, StorageError> {
    Ok(sqlx::query_scalar!(
        r#"
        SELECT COALESCE(-sum(e.amount), 0) AS "sum!"
          FROM ledger_entries e JOIN ledger_accounts a ON a.id = e.account_id
         WHERE a.code = $1 AND e.tx_id > $2 AND e.tx_id <= $3
        "#,
        code,
        after,
        upto
    )
    .fetch_one(conn)
    .await?)
}

/// Остаток счёта долга (кредитовое сальдо) по проводкам до `upto` включительно.
pub async fn credit_balance_at(
    conn: &mut PgConnection,
    code: &str,
    upto: i64,
) -> Result<Decimal, StorageError> {
    Ok(sqlx::query_scalar!(
        r#"
        SELECT COALESCE(-sum(e.amount), 0) AS "sum!"
          FROM ledger_entries e JOIN ledger_accounts a ON a.id = e.account_id
         WHERE a.code = $1 AND e.tx_id <= $2
        "#,
        code,
        upto
    )
    .fetch_one(conn)
    .await?)
}

/// Сколько USDT было на всех кошельках по проводкам до `upto` включительно — весь капитал
/// в обороте, знаменатель доли инвестора.
pub async fn wallets_usdt_at(conn: &mut PgConnection, upto: i64) -> Result<Decimal, StorageError> {
    Ok(sqlx::query_scalar!(
        r#"
        SELECT COALESCE(sum(e.amount), 0) AS "sum!"
          FROM ledger_entries e JOIN ledger_accounts a ON a.id = e.account_id
         WHERE a.type = 'asset' AND a.wallet_account_id IS NOT NULL AND a.asset = 'USDT'
           AND e.tx_id <= $1
        "#,
        upto
    )
    .fetch_one(conn)
    .await?)
}

/// Клиент с таким Telegram ID уже писал боту (`users`): доступ выдаётся только им.
pub async fn user_exists(conn: &mut PgConnection, user_id: i64) -> Result<bool, StorageError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM users WHERE tg_user_id = $1) AS "exists!""#,
        user_id
    )
    .fetch_one(conn)
    .await?)
}
