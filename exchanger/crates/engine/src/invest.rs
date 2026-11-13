//! Инвесторы (DESIGN-v0.2 Р18): пополнение чеком, вывод чеком, начисление прибыли.
//!
//! Правила денег — те же, что у заявок (CLAUDE.md п. 1–10):
//! - пополнение: операция `activate_check` в outbox в той же транзакции, что и движение
//!   (`invest_moves`); один чек — одна активация (`operations_check_once`), повтор — тем же
//!   `random_id`; зачислено — проводка `invest_deposit` (Дт кошелёк / Кт долг инвестору);
//! - вывод: доступно к выводу = баланс − пополнения моложе срока блокировки; одна открытая
//!   заявка на инвестора (`invest_moves_one_open_withdrawal`); в пределах автолимита — сразу,
//!   иначе решение владельца; одобрено — удержание резерва и `create_payout_check` с одной
//!   попыткой; чек создан — проводка `invest_withdrawal`; неясный исход — сверка
//!   `find_issued_check`, затем человек, **никогда** не повтор;
//! - прибыль: раз в `invest_accrual_every` выручка комиссий с прошлого начисления делится
//!   по долям капитала: инвестору — `выручка × его баланс на начало окна / весь капитал на
//!   кошельках × его % / 100`, вниз до 6 знаков. Проводка `invest_accrual`
//!   (Дт expense:investor_share / Кт долг инвестору) — прибыль капитализируется. Деньги,
//!   внесённые внутри окна, в нём не участвуют.

use chrono::Utc;
use domain::fsm::{MoneyRole, OpKind};
use domain::ledger::{self, AccountCode, Expense, Revenue};
use domain::{Asset, Decimal, Money, Platform};
use parsers::links::{LinkKind, WalletLink};
use rust_decimal::RoundingStrategy;
use serde_json::json;
use sqlx::PgConnection;
use storage::StorageError;
use storage::alerts::{Level, NewAlert, Topic};
use storage::audit::NewAudit;
use storage::directions::Side;
use storage::invest::{Figures, Investor, Move, MoveKind, MoveStatus};
use storage::ledger::PostingRefs;
use storage::operations::{NewInvestOperation, OpStatus, OperationRow};
use storage::staff::{Role, StaffMember};
use userbot::{ActivationOutcome, BotError, IssueOutcome, IssuedCheck, RejectReason};

use crate::worker::{older_than, reject_code, tag};
use crate::{Engine, EngineError};

/// Движение инвестора изменилось — бот сообщит ему.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvestUpdate {
    pub move_id: i64,
    pub user_id: i64,
}

/// Почему действие не принято. Деньги не тронуты.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvestRefusal {
    #[error("not an investor")]
    NotInvestor,
    #[error("send a check link")]
    NotACheck,
    #[error("this check was already used")]
    CheckUsed,
    #[error("no active wallet on this platform")]
    NoWallet,
    #[error("bad amount")]
    BadAmount,
    #[error("only {max} can be withdrawn now")]
    OverWithdrawable { max: Decimal },
    #[error("an open withdrawal already exists")]
    OpenWithdrawal,
    #[error("the move is not waiting for this decision")]
    WrongState,
    #[error("not enough free reserve: {available} available, {needed} needed")]
    NoReserve { available: Decimal, needed: Decimal },
    #[error("needs role {0:?}")]
    Forbidden(Role),
    #[error("amount {amount} is above the manual limit {limit}")]
    OverManualLimit { amount: Decimal, limit: Decimal },
    #[error("reason is too short")]
    ReasonTooShort,
    #[error("bad terms: {0}")]
    BadTerms(&'static str),
    #[error("unknown user")]
    UnknownUser,
}

#[derive(Debug, thiserror::Error)]
pub enum InvestError {
    #[error(transparent)]
    Refused(#[from] InvestRefusal),
    #[error(transparent)]
    Engine(#[from] EngineError),
}

impl From<StorageError> for InvestError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::OpenWithdrawal => InvestRefusal::OpenWithdrawal.into(),
            StorageError::CheckActivatedBefore => InvestRefusal::CheckUsed.into(),
            StorageError::InsufficientReserve { available, needed } => {
                InvestRefusal::NoReserve { available, needed }.into()
            }
            other => InvestError::Engine(other.into()),
        }
    }
}

impl From<sqlx::Error> for InvestError {
    fn from(e: sqlx::Error) -> Self {
        InvestError::Engine(e.into())
    }
}

impl From<domain::ledger::PostingError> for InvestError {
    fn from(e: domain::ledger::PostingError) -> Self {
        InvestError::Engine(e.into())
    }
}

/// Итог начисления прибыли.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Accrual {
    /// Выручка комиссий в окне.
    pub revenue: Decimal,
    /// Весь капитал на кошельках на начало окна (знаменатель доли).
    pub capital: Decimal,
    /// Кому сколько начислено.
    pub credited: Vec<(i64, Decimal)>,
}

/// Экран инвестора: условия, цифры и последние движения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvestView {
    pub investor: Investor,
    pub figures: Figures,
    pub recent: Vec<Move>,
}

/// Решение человека по движению с неясным исходом.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvestVerdict {
    /// Деньги пришли (пополнение — сколько USDT) или чек создан (вывод).
    Happened { amount: Option<Decimal> },
    /// Ничего не произошло: пополнение не зачислено, вывод возвращается на баланс.
    DidNotHappen,
}

const ACCRUAL_KEY: &str = "invest_accrual";
/// Ключ advisory-блокировки начисления: два прохода одновременно не начислят дважды.
const ACCRUAL_LOCK: (i32, i32) = (7003, 1);
/// Инвестиции — только в USDT.
const ASSET: Asset = Asset::Usdt;

/// Доля инвестора в выручке окна, вниз до 6 знаков.
pub fn investor_share(
    revenue: Decimal,
    balance: Decimal,
    capital: Decimal,
    share_pct: Decimal,
) -> Decimal {
    if revenue <= Decimal::ZERO || balance <= Decimal::ZERO || capital <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    // Долг инвестору не может быть больше всего капитала — ограничим на случай расхождения.
    let part = (balance / capital).min(Decimal::ONE);
    (revenue * part * share_pct / Decimal::ONE_HUNDRED)
        .round_dp_with_strategy(6, RoundingStrategy::ToZero)
        .max(Decimal::ZERO)
}

async fn ensure_investor_accounts(
    conn: &mut PgConnection,
    user_id: i64,
) -> Result<(), EngineError> {
    storage::ledger::ensure_account(
        &mut *conn,
        &AccountCode::investor(user_id, ASSET),
        ASSET,
        None,
    )
    .await?;
    storage::ledger::ensure_account(
        &mut *conn,
        &AccountCode::expense(Expense::InvestorShare, ASSET),
        ASSET,
        None,
    )
    .await?;
    Ok(())
}

fn alert_payload(m: &Move) -> serde_json::Value {
    json!({
        "move_id": m.id,
        "user_id": m.user_id,
        "kind": m.kind.as_db_str(),
        "platform": m.platform.db_name(),
        "amount": m.amount.map(|a| a.normalize().to_string()),
        "note": m.note,
    })
}

fn require(staff: &StaffMember, role: Role) -> Result<(), InvestRefusal> {
    if staff.active() && staff.role >= role {
        Ok(())
    } else {
        Err(InvestRefusal::Forbidden(role))
    }
}

impl Engine {
    pub fn subscribe_invest(&self) -> tokio::sync::broadcast::Receiver<InvestUpdate> {
        self.invest_updates.subscribe()
    }

    fn notify_invest(&self, m: &Move) {
        let _ = self.invest_updates.send(InvestUpdate {
            move_id: m.id,
            user_id: m.user_id,
        });
    }

    /// Экран инвестора; `None` — доступа нет (не выдан или отключён).
    pub async fn invest_view(&self, user_id: i64) -> Result<Option<InvestView>, EngineError> {
        let mut conn = self.pool.acquire().await?;
        let Some(investor) = storage::invest::active(&mut conn, user_id).await? else {
            return Ok(None);
        };
        let figures = storage::invest::figures(&mut conn, user_id, investor.lock_days).await?;
        let recent = storage::invest::recent(&mut conn, user_id, 5).await?;
        Ok(Some(InvestView {
            investor,
            figures,
            recent,
        }))
    }

    /// Выдать доступ или поменять условия (только владелец).
    pub async fn invest_save_terms(
        &self,
        staff: &StaffMember,
        terms: &Investor,
    ) -> Result<(), InvestError> {
        require(staff, Role::Owner)?;
        if terms.share_pct < Decimal::ZERO || terms.share_pct > Decimal::ONE_HUNDRED {
            return Err(InvestRefusal::BadTerms("share_pct").into());
        }
        if !(0..=365).contains(&terms.lock_days) {
            return Err(InvestRefusal::BadTerms("lock_days").into());
        }
        if terms.min_deposit <= Decimal::ZERO {
            return Err(InvestRefusal::BadTerms("min_deposit").into());
        }
        if terms.auto_withdraw_max < Decimal::ZERO {
            return Err(InvestRefusal::BadTerms("auto_withdraw_max").into());
        }
        let mut tx = self.pool.begin().await?;
        if !storage::invest::user_exists(&mut tx, terms.user_id).await? {
            return Err(InvestRefusal::UnknownUser.into());
        }
        let before = storage::invest::investor(&mut tx, terms.user_id).await?;
        storage::invest::save(&mut tx, terms, staff.tg_user_id).await?;
        ensure_investor_accounts(&mut tx, terms.user_id).await?;
        let target = terms.user_id.to_string();
        let as_json = |i: &Investor| {
            json!({
                "share_pct": i.share_pct.normalize().to_string(),
                "lock_days": i.lock_days,
                "min_deposit": i.min_deposit.normalize().to_string(),
                "auto_withdraw_max": i.auto_withdraw_max.normalize().to_string(),
                "is_active": i.is_active,
            })
        };
        storage::audit::record(
            &mut tx,
            &NewAudit {
                staff_id: staff.tg_user_id,
                action: "invest.terms",
                target_type: Some("user"),
                target_id: Some(&target),
                before: before.as_ref().map(as_json),
                after: Some(as_json(terms)),
                reason: None,
                double_confirmed: false,
            },
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Инвестор прислал чек на пополнение.
    pub async fn invest_deposit(
        &self,
        user_id: i64,
        link: &WalletLink,
    ) -> Result<i64, InvestError> {
        if link.kind != LinkKind::Check {
            return Err(InvestRefusal::NotACheck.into());
        }
        let mut tx = self.pool.begin().await?;
        storage::invest::active(&mut tx, user_id)
            .await?
            .ok_or(InvestRefusal::NotInvestor)?;
        if storage::operations::check_activated_before(&mut tx, link.platform, &link.param).await? {
            return Err(InvestRefusal::CheckUsed.into());
        }
        let (wallet, _) = storage::wallets::active(&mut tx, link.platform)
            .await?
            .ok_or(InvestRefusal::NoWallet)?;
        let move_id =
            storage::invest::insert_deposit(&mut tx, user_id, link.platform, &link.param).await?;
        let key = format!("inv-{move_id}-in-1");
        storage::operations::insert_invest(
            &mut tx,
            &NewInvestOperation {
                invest_move_id: move_id,
                kind: OpKind::ActivateCheck,
                role: MoneyRole::Intake,
                platform: link.platform,
                wallet_account_id: wallet,
                idempotency_key: &key,
                start_param: Some(&link.param),
                amount: None,
                asset: ASSET,
                random_id: userbot::flows::rid::new_random_id(),
            },
        )
        .await?;
        tx.commit().await?;
        tracing::info!(
            move_id,
            user_id,
            platform = link.platform.db_name(),
            "investor deposit accepted"
        );
        Ok(move_id)
    }

    /// Заявка на вывод. В пределах автолимита инвестора — сразу в работу, иначе (или если
    /// свободного резерва сейчас нет) — решение владельца (событие в админ-панель).
    pub async fn invest_withdraw(
        &self,
        user_id: i64,
        platform: Platform,
        amount: Decimal,
    ) -> Result<Move, InvestError> {
        if amount <= Decimal::ZERO || amount.scale() > 6 {
            return Err(InvestRefusal::BadAmount.into());
        }
        let mut tx = self.pool.begin().await?;
        let investor = storage::invest::active(&mut tx, user_id)
            .await?
            .ok_or(InvestRefusal::NotInvestor)?;
        let figures = storage::invest::figures(&mut tx, user_id, investor.lock_days).await?;
        if amount > figures.withdrawable() {
            return Err(InvestRefusal::OverWithdrawable {
                max: figures.withdrawable(),
            }
            .into());
        }
        storage::wallets::active(&mut tx, platform)
            .await?
            .ok_or(InvestRefusal::NoWallet)?;
        let move_id =
            storage::invest::insert_withdrawal(&mut tx, user_id, platform, amount).await?;
        let m = storage::invest::get(&mut tx, move_id, true).await?;
        if amount <= investor.auto_withdraw_max {
            // Отказ здесь — не ошибка заявки: её решит владелец. Ошибки базы — наверх.
            // Точка сохранения: частично сделанный старт не должен остаться в транзакции.
            sqlx::query("SAVEPOINT invest_auto")
                .execute(&mut *tx)
                .await?;
            match self.start_withdrawal(&mut tx, &m, None).await {
                Ok(()) => {}
                Err(InvestError::Refused(r)) => {
                    sqlx::query("ROLLBACK TO SAVEPOINT invest_auto")
                        .execute(&mut *tx)
                        .await?;
                    tracing::info!(move_id, reason = %r, "auto withdrawal not possible, the owner decides");
                }
                Err(e) => return Err(e),
            }
        }
        let m = storage::invest::get(&mut tx, move_id, false).await?;
        if m.status == MoveStatus::Requested {
            storage::alerts::raise(
                &mut tx,
                &NewAlert {
                    level: Level::Warn,
                    topic: Topic::Reserves,
                    dedup_key: &format!("invest_withdrawal:{move_id}"),
                    title: "invest_withdrawal",
                    payload: alert_payload(&m),
                },
            )
            .await?;
        }
        tx.commit().await?;
        tracing::info!(move_id, user_id, %amount, status = m.status.as_db_str(), "investor withdrawal requested");
        self.notify_invest(&m);
        Ok(m)
    }

    /// Одобрить вывод: владелец; админ — в пределах своего потолка ручных выплат.
    pub async fn invest_approve(
        &self,
        move_id: i64,
        staff: &StaffMember,
    ) -> Result<Move, InvestError> {
        require(staff, Role::Admin)?;
        let mut tx = self.pool.begin().await?;
        let m = storage::invest::get(&mut tx, move_id, true).await?;
        if m.kind != MoveKind::Withdrawal || m.status != MoveStatus::Requested {
            return Err(InvestRefusal::WrongState.into());
        }
        let amount = m.amount.ok_or(InvestRefusal::BadAmount)?;
        if let Some(limit) = staff.manual_payout_limit()
            && amount > limit
        {
            return Err(InvestRefusal::OverManualLimit { amount, limit }.into());
        }
        self.start_withdrawal(&mut tx, &m, Some(staff.tg_user_id))
            .await?;
        storage::alerts::resolve(&mut tx, &format!("invest_withdrawal:{move_id}")).await?;
        let target = move_id.to_string();
        storage::audit::record(
            &mut tx,
            &NewAudit {
                staff_id: staff.tg_user_id,
                action: "invest.approve",
                target_type: Some("invest_move"),
                target_id: Some(&target),
                before: None,
                after: Some(json!({ "amount": amount.to_string(), "user_id": m.user_id })),
                reason: None,
                double_confirmed: true,
            },
        )
        .await?;
        let m = storage::invest::get(&mut tx, move_id, false).await?;
        tx.commit().await?;
        self.notify_invest(&m);
        Ok(m)
    }

    /// Отклонить вывод: деньги остаются в обороте у инвестора.
    pub async fn invest_reject(
        &self,
        move_id: i64,
        staff: &StaffMember,
        reason: &str,
    ) -> Result<Move, InvestError> {
        require(staff, Role::Admin)?;
        let reason = reason.trim();
        if reason.chars().count() < 3 {
            return Err(InvestRefusal::ReasonTooShort.into());
        }
        let mut tx = self.pool.begin().await?;
        let m = storage::invest::get(&mut tx, move_id, true).await?;
        if m.kind != MoveKind::Withdrawal || m.status != MoveStatus::Requested {
            return Err(InvestRefusal::WrongState.into());
        }
        storage::invest::set_status(
            &mut tx,
            move_id,
            MoveStatus::Rejected,
            None,
            None,
            Some(reason),
            Some(staff.tg_user_id),
        )
        .await?;
        storage::alerts::resolve(&mut tx, &format!("invest_withdrawal:{move_id}")).await?;
        let target = move_id.to_string();
        storage::audit::record(
            &mut tx,
            &NewAudit {
                staff_id: staff.tg_user_id,
                action: "invest.reject",
                target_type: Some("invest_move"),
                target_id: Some(&target),
                before: None,
                after: None,
                reason: Some(reason),
                double_confirmed: true,
            },
        )
        .await?;
        let m = storage::invest::get(&mut tx, move_id, false).await?;
        tx.commit().await?;
        self.notify_invest(&m);
        Ok(m)
    }

    /// Решение владельца по движению с неясным исходом (`manual`): что было на самом деле.
    pub async fn invest_resolve(
        &self,
        move_id: i64,
        staff: &StaffMember,
        verdict: &InvestVerdict,
        reason: &str,
    ) -> Result<Move, InvestError> {
        require(staff, Role::Owner)?;
        let reason = reason.trim();
        if reason.chars().count() < 3 {
            return Err(InvestRefusal::ReasonTooShort.into());
        }
        let mut tx = self.pool.begin().await?;
        let m = storage::invest::get(&mut tx, move_id, true).await?;
        if m.status != MoveStatus::Manual {
            return Err(InvestRefusal::WrongState.into());
        }
        let op = storage::operations::for_invest_move(&mut tx, move_id)
            .await?
            .pop();
        let created_by = format!("staff:{}", staff.tg_user_id);
        let open_op = op
            .as_ref()
            .filter(|o| matches!(o.status, OpStatus::Unknown | OpStatus::Failed));
        let happened = matches!(verdict, InvestVerdict::Happened { .. });
        if let Some(o) = open_op {
            storage::operations::resolve_by_staff(
                &mut tx,
                o.id,
                if happened {
                    OpStatus::Succeeded
                } else {
                    OpStatus::Failed
                },
                &json!({ "resolved_by": created_by, "happened": happened, "reason": reason }),
                None,
            )
            .await?;
        }
        ensure_investor_accounts(&mut tx, m.user_id).await?;
        let refs = PostingRefs {
            operation_id: open_op.map(|o| o.id),
            memo: Some(reason),
            created_by: Some(&created_by),
            ..Default::default()
        };
        let (status, amount) = match (m.kind, verdict) {
            (MoveKind::Deposit, InvestVerdict::Happened { amount }) => {
                let amount = amount
                    .filter(|a| *a > Decimal::ZERO)
                    .ok_or(InvestRefusal::BadAmount)?;
                storage::ledger::post(
                    &mut tx,
                    &ledger::invest_deposit(m.platform, ASSET, m.user_id, amount)?,
                    refs,
                )
                .await?;
                (MoveStatus::Done, Some(amount))
            }
            (MoveKind::Withdrawal, InvestVerdict::Happened { .. }) => {
                let amount = m.amount.ok_or(InvestRefusal::BadAmount)?;
                storage::ledger::post(
                    &mut tx,
                    &ledger::invest_withdrawal(m.platform, ASSET, m.user_id, amount)?,
                    refs,
                )
                .await?;
                storage::holds::close_invest(&mut tx, move_id, true).await?;
                (MoveStatus::Done, None)
            }
            (MoveKind::Withdrawal, InvestVerdict::DidNotHappen) => {
                storage::holds::close_invest(&mut tx, move_id, false).await?;
                (MoveStatus::Failed, None)
            }
            (MoveKind::Deposit, InvestVerdict::DidNotHappen) => (MoveStatus::Failed, None),
        };
        storage::invest::set_status(
            &mut tx,
            move_id,
            status,
            amount,
            None,
            Some(reason),
            Some(staff.tg_user_id),
        )
        .await?;
        storage::alerts::resolve(&mut tx, &format!("invest_manual:{move_id}")).await?;
        let target = move_id.to_string();
        storage::audit::record(
            &mut tx,
            &NewAudit {
                staff_id: staff.tg_user_id,
                action: "invest.resolve",
                target_type: Some("invest_move"),
                target_id: Some(&target),
                before: None,
                after: Some(json!({
                    "happened": happened,
                    "amount": amount.map(|a| a.to_string()),
                })),
                reason: Some(reason),
                double_confirmed: true,
            },
        )
        .await?;
        let m = storage::invest::get(&mut tx, move_id, false).await?;
        // Пополнение, чья активация осталась без ответа (Ю-2), или вывод, чья сумма чека осталась
        // без ответа (шаг 1.2), держали кошелёк на паузе.
        let unanswered = open_op
            .filter(|o| {
                o.status == OpStatus::Unknown
                    && matches!(o.kind, OpKind::ActivateCheck | OpKind::CreatePayoutCheck)
            })
            .map(|o| o.platform);
        if let Some(platform) = unanswered {
            storage::directions::clear_auto_pause_reason(
                &mut tx,
                platform,
                crate::UNANSWERED_PAUSE,
            )
            .await?;
        }
        tx.commit().await?;
        if let Some(platform) = unanswered {
            self.unanswered_resolved(platform).await;
        }
        self.notify_invest(&m);
        Ok(m)
    }

    /// Удержание резерва и операция вывода — в транзакции вызывающего, движение заблокировано.
    async fn start_withdrawal(
        &self,
        conn: &mut PgConnection,
        m: &Move,
        staff: Option<i64>,
    ) -> Result<(), InvestError> {
        let amount = m.amount.ok_or(InvestRefusal::BadAmount)?;
        // Заново: пока заявка ждала, баланс и блокировка могли измениться.
        let investor = storage::invest::investor(&mut *conn, m.user_id)
            .await?
            .ok_or(InvestRefusal::NotInvestor)?;
        let figures = storage::invest::figures(&mut *conn, m.user_id, investor.lock_days).await?;
        if amount > figures.withdrawable() {
            return Err(InvestRefusal::OverWithdrawable {
                max: figures.withdrawable(),
            }
            .into());
        }
        let (wallet, _) = storage::wallets::active(&mut *conn, m.platform)
            .await?
            .ok_or(InvestRefusal::NoWallet)?;
        storage::holds::lock_wallet(&mut *conn, wallet).await?;
        // Свободный резерв с учётом «лимита без код-пароля» CryptoBot (DESIGN-v0.2 Р11).
        let free =
            crate::submit::spendable(&mut *conn, &self.cfg, m.platform, wallet, ASSET).await?;
        if free < amount {
            return Err(InvestRefusal::NoReserve {
                available: free,
                needed: amount,
            }
            .into());
        }
        storage::holds::place_invest(&mut *conn, m.id, wallet, ASSET, amount).await?;
        let key = format!("inv-{}-out-1", m.id);
        storage::operations::insert_invest(
            &mut *conn,
            &NewInvestOperation {
                invest_move_id: m.id,
                kind: OpKind::CreatePayoutCheck,
                role: MoneyRole::Settle,
                platform: m.platform,
                wallet_account_id: wallet,
                idempotency_key: &key,
                start_param: None,
                amount: Some(amount),
                asset: ASSET,
                random_id: userbot::flows::rid::new_random_id(),
            },
        )
        .await?;
        storage::invest::set_status(
            &mut *conn,
            m.id,
            MoveStatus::Processing,
            None,
            None,
            None,
            staff,
        )
        .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // Исходы юзербота (из исполнителя outbox)

    pub(crate) async fn invest_on_activation(
        &self,
        op: &OperationRow,
        move_id: i64,
        result: Result<ActivationOutcome, BotError>,
    ) -> Result<(), EngineError> {
        let mut outcome = match result {
            Ok(o) => o,
            Err(e) => return self.release(op, &e).await,
        };
        // Как у заявок: «уже активирован» на повторной попытке мог означать нашу прошлую.
        let repeated_loss = op.attempts > 1
            && matches!(
                outcome,
                ActivationOutcome::Rejected(
                    RejectReason::AlreadyActivated | RejectReason::NotFound
                )
            );
        if repeated_loss {
            outcome = ActivationOutcome::Unknown { raw: None };
        }
        if let ActivationOutcome::Unknown { raw } = &outcome
            && !repeated_loss
        {
            let detail = raw.as_ref().map_or("no reply", |m| m.text.as_str());
            let at = Utc::now()
                + chrono::Duration::from_std(self.cfg.activation_retry_backoff).unwrap_or_default();
            let mut conn = self.pool.acquire().await?;
            if storage::operations::retry(&mut conn, op.id, at, detail).await? {
                return Ok(());
            }
        }
        let mut tx = self.pool.begin().await?;
        let m = storage::invest::get(&mut tx, move_id, true).await?;
        match outcome {
            ActivationOutcome::Received(money) if money.asset() == ASSET => {
                storage::operations::finish(
                    &mut tx,
                    op.id,
                    OpStatus::Succeeded,
                    &json!({ "received": money.amount().to_string(), "asset": money.asset().code() }),
                    None,
                    None,
                )
                .await?;
                ensure_investor_accounts(&mut tx, m.user_id).await?;
                storage::ledger::post(
                    &mut tx,
                    &ledger::invest_deposit(op.platform, ASSET, m.user_id, money.amount())?,
                    PostingRefs {
                        operation_id: Some(op.id),
                        memo: Some("пополнение инвестора"),
                        ..Default::default()
                    },
                )
                .await?;
                storage::invest::set_status(
                    &mut tx,
                    move_id,
                    MoveStatus::Done,
                    Some(money.amount()),
                    None,
                    None,
                    None,
                )
                .await?;
                tracing::info!(move_id, user = m.user_id, amount = %money.amount(), "investor deposit credited");
            }
            ActivationOutcome::Received(money) => {
                storage::operations::finish(
                    &mut tx,
                    op.id,
                    OpStatus::Succeeded,
                    &json!({ "received_other": money.amount().to_string(), "code": money.asset().code() }),
                    None,
                    None,
                )
                .await?;
                let note = format!(
                    "чек пополнения в {}, а не в USDT: {} в кошельке, но не в леджере — верните инвестору вручную",
                    money.asset().code(),
                    money.amount()
                );
                self.invest_manual(&mut tx, &m, &note).await?;
            }
            ActivationOutcome::ReceivedOther { amount, code } => {
                storage::operations::finish(
                    &mut tx,
                    op.id,
                    OpStatus::Succeeded,
                    &json!({ "received_other": amount.to_string(), "code": code }),
                    None,
                    None,
                )
                .await?;
                let note = format!(
                    "чек пополнения в {code}, а не в USDT: {amount} {code} в кошельке, но не в леджере — верните инвестору вручную"
                );
                self.invest_manual(&mut tx, &m, &note).await?;
            }
            ActivationOutcome::Rejected(reason) => {
                let code = reject_code(reason);
                storage::operations::finish(
                    &mut tx,
                    op.id,
                    OpStatus::Failed,
                    &json!({ "rejected": code }),
                    None,
                    Some(code),
                )
                .await?;
                storage::invest::set_status(
                    &mut tx,
                    move_id,
                    MoveStatus::Failed,
                    None,
                    None,
                    Some(code),
                    None,
                )
                .await?;
            }
            ActivationOutcome::Unknown { raw } => {
                let unrecognised = raw.is_some();
                storage::operations::finish(
                    &mut tx,
                    op.id,
                    OpStatus::Unknown,
                    &json!({ "reply": raw.map(|m| m.text), "repeated_loss": repeated_loss }),
                    None,
                    Some("activation outcome unknown"),
                )
                .await?;
                let note = if repeated_loss {
                    "повторная активация говорит, что чека уже нет: возможно, его забрала наша прежняя попытка — проверьте кошелёк"
                } else {
                    "исход активации чека пополнения неизвестен: проверьте чат кошелька и баланс"
                };
                self.invest_manual(&mut tx, &m, note).await?;
                if !unrecognised && !repeated_loss {
                    Self::pause_unanswered(&mut tx, op.platform).await?;
                }
                if unrecognised {
                    storage::directions::auto_pause_platform(
                        &mut tx,
                        op.platform,
                        Side::Source,
                        "незнакомый ответ кошелька на активацию чека",
                    )
                    .await?;
                }
            }
        }
        let m = storage::invest::get(&mut tx, move_id, false).await?;
        tx.commit().await?;
        self.notify_invest(&m);
        Ok(())
    }

    pub(crate) async fn invest_on_issue(
        &self,
        op: &OperationRow,
        move_id: i64,
        result: Result<IssueOutcome, BotError>,
    ) -> Result<(), EngineError> {
        let outcome = match result {
            Ok(o) => o,
            Err(e) => return self.release(op, &e).await,
        };
        let outcome_holds_chat = outcome.holds_chat();
        match outcome {
            IssueOutcome::Issued { check, .. } => self.invest_issued(op, move_id, &check).await,
            IssueOutcome::InsufficientFunds => {
                let reason = "в кошельке не хватило денег на чек вывода";
                let mut tx = self.pool.begin().await?;
                let m = storage::invest::get(&mut tx, move_id, true).await?;
                storage::operations::finish(
                    &mut tx,
                    op.id,
                    OpStatus::Failed,
                    &json!({ "insufficient_funds": true }),
                    None,
                    Some(reason),
                )
                .await?;
                storage::holds::close_invest(&mut tx, move_id, false).await?;
                storage::invest::set_status(
                    &mut tx,
                    move_id,
                    MoveStatus::Failed,
                    None,
                    None,
                    Some(reason),
                    None,
                )
                .await?;
                storage::directions::auto_pause_platform(
                    &mut tx,
                    op.platform,
                    Side::Target,
                    reason,
                )
                .await?;
                self.invest_alert(&mut tx, &m, "invest_failed", Level::Warn)
                    .await?;
                let m = storage::invest::get(&mut tx, move_id, false).await?;
                tx.commit().await?;
                self.notify_invest(&m);
                Ok(())
            }
            IssueOutcome::Unknown { detail } => {
                {
                    let mut conn = self.pool.acquire().await?;
                    // Сумма чека без ответа: чат кошелька занят (шаг 1.2).
                    if outcome_holds_chat {
                        Self::pause_unanswered(&mut conn, op.platform).await?;
                    }
                    match storage::operations::finish(
                        &mut conn,
                        op.id,
                        OpStatus::Unknown,
                        &json!({ "detail": detail }),
                        None,
                        Some(&detail),
                    )
                    .await
                    {
                        Ok(()) | Err(StorageError::OperationState(_)) => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                tracing::warn!(op = op.id, move_id, %detail, "investor withdrawal outcome unknown, reconciling");
                self.invest_reconcile(op, move_id).await
            }
        }
    }

    async fn invest_issued(
        &self,
        op: &OperationRow,
        move_id: i64,
        check: &IssuedCheck,
    ) -> Result<(), EngineError> {
        let mut tx = self.pool.begin().await?;
        let m = storage::invest::get(&mut tx, move_id, true).await?;
        storage::operations::finish(
            &mut tx,
            op.id,
            OpStatus::Succeeded,
            &json!({ "url": check.url }),
            Some(&check.param),
            None,
        )
        .await?;
        ensure_investor_accounts(&mut tx, m.user_id).await?;
        let amount = op.amount.unwrap_or_else(|| check.amount.amount());
        storage::ledger::post(
            &mut tx,
            &ledger::invest_withdrawal(op.platform, ASSET, m.user_id, amount)?,
            PostingRefs {
                operation_id: Some(op.id),
                memo: Some("вывод инвестора"),
                ..Default::default()
            },
        )
        .await?;
        storage::holds::close_invest(&mut tx, move_id, true).await?;
        storage::invest::set_status(
            &mut tx,
            move_id,
            MoveStatus::Done,
            None,
            Some(&check.url),
            None,
            None,
        )
        .await?;
        let m = storage::invest::get(&mut tx, move_id, false).await?;
        tx.commit().await?;
        tracing::info!(move_id, user = m.user_id, %amount, "investor withdrawal check issued");
        self.notify_invest(&m);
        Ok(())
    }

    /// Неясный исход вывода: найти чек; не нашли за `reconcile_grace` — человеку.
    pub(crate) async fn invest_reconcile(
        &self,
        op: &OperationRow,
        move_id: i64,
    ) -> Result<(), EngineError> {
        let amount = op
            .amount
            .ok_or_else(|| EngineError::Invariant(format!("operation {} has no amount", op.id)))?;
        let money = Money::new(amount, op.asset.unwrap_or(ASSET))?;
        let wallet = self.wallet(op.platform)?.clone();
        match wallet.find_issued_check(&money, &tag(op)).await {
            Ok(Some(check)) => self.invest_issued(op, move_id, &check).await,
            Ok(None) if !older_than(op, self.cfg.reconcile_grace) => Ok(()),
            Ok(None) => {
                self.invest_give_up(
                    op,
                    move_id,
                    "исход создания чека вывода неизвестен и чек не найден: проверьте в кошельке",
                )
                .await
            }
            Err(e) if older_than(op, self.cfg.escalate_after) => {
                self.invest_give_up(
                    op,
                    move_id,
                    &format!("исход создания чека вывода неизвестен, сверка невозможна: {e}"),
                )
                .await
            }
            Err(e) => {
                tracing::warn!(op = op.id, error = %e, "investor withdrawal reconciliation postponed");
                Ok(())
            }
        }
    }

    /// Автоматика сдалась: удержание держит деньги, движение — у человека.
    async fn invest_give_up(
        &self,
        op: &OperationRow,
        move_id: i64,
        reason: &str,
    ) -> Result<(), EngineError> {
        let mut tx = self.pool.begin().await?;
        let m = storage::invest::get(&mut tx, move_id, true).await?;
        if m.status == MoveStatus::Processing {
            self.invest_manual(&mut tx, &m, reason).await?;
        }
        storage::operations::give_up(&mut tx, op.id, reason).await?;
        storage::directions::auto_pause_platform(&mut tx, op.platform, Side::Target, reason)
            .await?;
        let m = storage::invest::get(&mut tx, move_id, false).await?;
        tx.commit().await?;
        tracing::error!(op = op.id, move_id, %reason, "investor move handed to a human");
        self.notify_invest(&m);
        Ok(())
    }

    /// Движение — к человеку (статус `manual` + событие в админ-панель).
    pub(crate) async fn invest_manual(
        &self,
        conn: &mut PgConnection,
        m: &Move,
        note: &str,
    ) -> Result<(), EngineError> {
        storage::invest::set_status(
            &mut *conn,
            m.id,
            MoveStatus::Manual,
            None,
            None,
            Some(note),
            None,
        )
        .await?;
        let mut m = m.clone();
        m.note = Some(note.to_owned());
        self.invest_alert(conn, &m, "invest_manual", Level::Critical)
            .await
    }

    async fn invest_alert(
        &self,
        conn: &mut PgConnection,
        m: &Move,
        title: &str,
        level: Level,
    ) -> Result<(), EngineError> {
        storage::alerts::raise(
            conn,
            &NewAlert {
                level,
                topic: Topic::Errors,
                dedup_key: &format!("{title}:{}", m.id),
                title,
                payload: alert_payload(m),
            },
        )
        .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // Прибыль

    /// Начислить инвесторам их долю выручки комиссий с прошлого начисления.
    pub async fn invest_accrue(&self) -> Result<Accrual, EngineError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
            .bind(ACCRUAL_LOCK.0)
            .bind(ACCRUAL_LOCK.1)
            .execute(&mut *tx)
            .await?;
        let last: i64 = storage::settings::get(&mut tx, ACCRUAL_KEY)
            .await?
            .and_then(|v| v.get("last_tx").and_then(serde_json::Value::as_i64))
            .unwrap_or(0);
        let upto = storage::invest::last_tx_id(&mut tx).await?;
        let mut out = Accrual::default();
        if upto <= last {
            return Ok(out);
        }
        let revenue_code = AccountCode::revenue(Revenue::Fee, ASSET);
        out.revenue =
            storage::invest::credit_turnover(&mut tx, revenue_code.as_str(), last, upto).await?;
        out.capital = storage::invest::wallets_usdt_at(&mut tx, last).await?;
        if out.revenue > Decimal::ZERO && out.capital > Decimal::ZERO {
            let memo = format!("прибыль инвестора за проводки {}–{upto}", last + 1);
            for inv in storage::invest::list(&mut tx).await? {
                if !inv.is_active || inv.share_pct <= Decimal::ZERO {
                    continue;
                }
                let code = AccountCode::investor(inv.user_id, ASSET);
                let balance =
                    storage::invest::credit_balance_at(&mut tx, code.as_str(), last).await?;
                let amount = investor_share(out.revenue, balance, out.capital, inv.share_pct);
                if amount <= Decimal::ZERO {
                    continue;
                }
                ensure_investor_accounts(&mut tx, inv.user_id).await?;
                storage::ledger::post(
                    &mut tx,
                    &ledger::invest_accrual(ASSET, inv.user_id, amount)?,
                    PostingRefs {
                        memo: Some(&memo),
                        ..Default::default()
                    },
                )
                .await?;
                out.credited.push((inv.user_id, amount));
            }
        }
        // Курсор — на последнюю проводку до начисления. Проводки начисления выручку не
        // трогают: следующее окно их не посчитает.
        storage::settings::set(&mut tx, ACCRUAL_KEY, &json!({ "last_tx": upto }), None).await?;
        tx.commit().await?;
        if !out.credited.is_empty() {
            tracing::info!(revenue = %out.revenue, capital = %out.capital, investors = out.credited.len(), "investor profit accrued");
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn share_follows_the_formula() {
        // 10 USDT выручки, у инвестора 1 000 из 10 000 капитала, доля 50 % → 0.5.
        assert_eq!(
            investor_share(dec!(10), dec!(1000), dec!(10000), dec!(50)),
            dec!(0.5)
        );
        // Округление вниз до 6 знаков.
        assert_eq!(
            investor_share(dec!(1), dec!(1), dec!(3), dec!(100)),
            dec!(0.333333)
        );
    }

    #[test]
    fn share_is_zero_without_revenue_or_capital() {
        assert_eq!(investor_share(dec!(0), dec!(1), dec!(1), dec!(50)), dec!(0));
        assert_eq!(
            investor_share(dec!(-5), dec!(1), dec!(1), dec!(50)),
            dec!(0)
        );
        assert_eq!(investor_share(dec!(5), dec!(0), dec!(1), dec!(50)), dec!(0));
        assert_eq!(investor_share(dec!(5), dec!(1), dec!(0), dec!(50)), dec!(0));
    }

    #[test]
    fn share_never_exceeds_revenue_times_pct() {
        // Баланс больше капитала (расхождение) — не больше всей выручки × доля.
        assert_eq!(
            investor_share(dec!(10), dec!(500), dec!(100), dec!(40)),
            dec!(4)
        );
    }
}
