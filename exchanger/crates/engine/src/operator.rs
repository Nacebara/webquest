//! Решения персонала по заявке на ручном разборе (SPEC §4 П9, §7.2, §7.4).
//!
//! Каждое решение — одна транзакция: проверки, переход заявки (`Event::Operator`), операция
//! outbox или проводка и запись в `admin_audit`. Правила, которые нельзя обойти:
//!
//! - решение только по заявке в `MANUAL_REVIEW` и только когда ни одна её операция не в работе
//!   (`pending` / `dispatched`);
//! - «Повторить выплату», «Повторить оплату», «Вернуть», «Закрыть: выплачено» — только если
//!   приход проведён в леджере (деньги клиента у нас). Не проведён — сначала «Деньги пришли»;
//! - успешная выплата или оплата по заявке уже есть — ни повтора, ни возврата, ни закрытия
//!   без выплаты (уникальный индекс `operations_one_settle_per_order` — последняя линия);
//! - прежняя попытка с неясным исходом (`unknown`) — повтор или возврат только после того, как
//!   сотрудник подтвердил: «проверил кошелёк — чек не создан / счёт не оплачен»; эта попытка
//!   помечается `failed` в той же транзакции;
//! - сумма ручной выплаты или возврата выше потолка сотрудника (оператор 50, админ 300 USDT) —
//!   только владелец;
//! - «Закрыть: выплачено» и «Деньги пришли» — админ или владелец; «Закрыть без выплаты» —
//!   только владелец; причина — всегда, не короче 3 символов.

use domain::fsm::{Event, Flow, MoneyRole, OpKind, OperatorAction, OrderState};
use domain::ledger::{self, TxKind};
use domain::{Asset, Decimal, Money};
use serde_json::{Value, json};
use sqlx::PgConnection;
use storage::StorageError;
use storage::audit::NewAudit;
use storage::operations::{OpStatus, OperationRow};
use storage::orders::{Actor, OrderPatch, OrderRow};
use storage::staff::{Role, StaffMember};

use crate::effects::{EffectCtx, transit};
use crate::settle::{Decision as Rule, decide, refund};
use crate::{Engine, EngineError};

/// Что решил сотрудник.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorDecision {
    /// Выплатить по правилам (обмен). `previous_failed` — проверил: прежний чек не создан.
    RetryPayout { previous_failed: bool },
    /// Оплатить счёт снова. `previous_failed` — проверил: счёт нами не оплачен.
    RetryInvoicePayment { previous_failed: bool },
    /// Вернуть клиенту то, что пришло. `previous_failed` — прежняя выплата точно не прошла.
    Refund { previous_failed: bool },
    /// Выплата есть: ссылка на созданный чек или описание доказательства.
    CloseAsPaid { evidence: String },
    /// Закрыть без выплаты (владелец).
    CloseWithoutPayout,
    /// Автоматика не увидела приход, сотрудник видит деньги в кошельке: сумма прихода.
    ConfirmIntake { amount: Decimal },
}

impl OperatorDecision {
    /// Ключ действия в журнале.
    pub fn audit_action(&self) -> &'static str {
        match self {
            OperatorDecision::RetryPayout { .. } => "order.retry_payout",
            OperatorDecision::RetryInvoicePayment { .. } => "order.retry_invoice_payment",
            OperatorDecision::Refund { .. } => "order.refund",
            OperatorDecision::CloseAsPaid { .. } => "order.close_as_paid",
            OperatorDecision::CloseWithoutPayout => "order.close_without_payout",
            OperatorDecision::ConfirmIntake { .. } => "order.confirm_intake",
        }
    }

    fn min_role(&self) -> Role {
        match self {
            OperatorDecision::CloseWithoutPayout => Role::Owner,
            OperatorDecision::CloseAsPaid { .. } | OperatorDecision::ConfirmIntake { .. } => {
                Role::Admin
            }
            _ => Role::Operator,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OperatorRequest<'a> {
    pub order_id: i64,
    pub decision: OperatorDecision,
    pub staff: &'a StaffMember,
    pub reason: &'a str,
    /// Пульт показал предпросмотр и сотрудник подтвердил (SPEC §7.4).
    pub double_confirmed: bool,
}

/// Почему решение не принято. Деньги и заявка не тронуты.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("order is not in manual review (state {0})")]
    NotInReview(OrderState),
    #[error("this decision does not fit this kind of order")]
    WrongFlow,
    #[error("needs role {0:?}")]
    Forbidden(Role),
    #[error("reason is too short")]
    ReasonTooShort,
    #[error("evidence is required")]
    EvidenceRequired,
    #[error("operation {0} is still in progress")]
    InFlight(String),
    #[error("the client's money is not posted as received")]
    FundsNotReceived,
    #[error("received less than the quote")]
    Underfunded,
    #[error("operation {0} already settled this order")]
    AlreadySettled(String),
    #[error("operation {0} has an unknown outcome: confirm it did not happen")]
    PreviousUnknown(String),
    #[error("amount {amount} is above the manual limit {limit}")]
    OverManualLimit { amount: Decimal, limit: Decimal },
    #[error("by the rules this order is not paid out: {0}")]
    Rules(String),
    #[error("not enough free reserve: {available} available, {needed} needed")]
    NoReserve { available: Decimal, needed: Decimal },
    #[error("the intake is already posted")]
    IntakeAlreadyPosted,
    #[error("no activation with an unknown outcome to confirm")]
    NoIntakeOperation,
    #[error("bad amount")]
    BadAmount,
    #[error("no payout attempt to close: use retry instead")]
    NoSettleAttempt,
    #[error("this check already settles another operation")]
    CheckAlreadyUsed,
    #[error("the order changed meanwhile, open it again")]
    Changed,
}

#[derive(Debug, thiserror::Error)]
pub enum OperatorError {
    #[error(transparent)]
    Refused(#[from] Refusal),
    #[error(transparent)]
    Engine(#[from] EngineError),
}

impl From<StorageError> for OperatorError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::Conflict { .. } => Refusal::Changed.into(),
            StorageError::CheckAlreadyUsed(_) => Refusal::CheckAlreadyUsed.into(),
            StorageError::InsufficientReserve { available, needed } => {
                Refusal::NoReserve { available, needed }.into()
            }
            other => OperatorError::Engine(other.into()),
        }
    }
}

impl From<domain::ledger::PostingError> for OperatorError {
    fn from(e: domain::ledger::PostingError) -> Self {
        OperatorError::Engine(e.into())
    }
}

impl From<domain::money::MoneyError> for OperatorError {
    fn from(e: domain::money::MoneyError) -> Self {
        OperatorError::Engine(e.into())
    }
}

impl From<sqlx::Error> for OperatorError {
    fn from(e: sqlx::Error) -> Self {
        OperatorError::Engine(e.into())
    }
}

fn settled_by(ops: &[OperationRow]) -> Option<&OperationRow> {
    ops.iter()
        .find(|o| o.role == MoneyRole::Settle && o.status == OpStatus::Succeeded)
}

fn unknown_settle(ops: &[OperationRow]) -> Vec<&OperationRow> {
    ops.iter()
        .filter(|o| o.role == MoneyRole::Settle && o.status == OpStatus::Unknown)
        .collect()
}

/// Прежние попытки с неясным исходом: без подтверждения — отказ; с ним — `failed`.
async fn retire_unknown(
    conn: &mut PgConnection,
    ops: &[OperationRow],
    confirmed: bool,
    staff: i64,
    reason: &str,
) -> Result<(), OperatorError> {
    let unknown = unknown_settle(ops);
    if let Some(op) = unknown.first()
        && !confirmed
    {
        return Err(Refusal::PreviousUnknown(op.idempotency_key.clone()).into());
    }
    for op in unknown {
        storage::operations::resolve_by_staff(
            &mut *conn,
            op.id,
            OpStatus::Failed,
            &json!({ "resolved_by": format!("staff:{staff}"), "verdict": "did_not_happen", "reason": reason }),
            None,
        )
        .await?;
    }
    Ok(())
}

fn check_limit(staff: &StaffMember, amount: Decimal) -> Result<(), Refusal> {
    match staff.manual_payout_limit() {
        Some(limit) if amount > limit => Err(Refusal::OverManualLimit { amount, limit }),
        _ => Ok(()),
    }
}

/// Удержание под выплату на кошельке выплаты: есть подходящее — оставить; на другом кошельке
/// (после неудачного возврата) или меньше нужного — заменить.
async fn ensure_target_hold(
    conn: &mut PgConnection,
    order: &OrderRow,
    asset: Asset,
    needed: Decimal,
) -> Result<(), OperatorError> {
    let target = order.direction.target();
    let (wallet, _) = storage::wallets::active(&mut *conn, target)
        .await?
        .ok_or(EngineError::NoWalletAccount(target))?;
    if let Some((w, _, amount)) = storage::holds::active(&mut *conn, order.id).await?
        && w == wallet
        && amount >= needed
    {
        return Ok(());
    }
    storage::holds::lock_wallet(&mut *conn, wallet).await?;
    storage::holds::close(&mut *conn, order.id, false).await?;
    storage::holds::place(&mut *conn, order.id, wallet, asset, needed).await?;
    Ok(())
}

/// Чек выплаты из доказательства: ссылка на чек платформы выплаты → его код.
fn check_param_from(evidence: &str, order: &OrderRow) -> Option<String> {
    evidence.split_whitespace().find_map(|word| {
        let link = parsers::links::parse_url(word).ok()?;
        (link.kind == parsers::links::LinkKind::Check && link.platform == order.direction.target())
            .then_some(link.param)
    })
}

fn snapshot(order: &OrderRow, d: &storage::orders::OrderDetails) -> Value {
    json!({
        "state": order.state.as_db_str(),
        "intake_amount": d.intake_amount.map(|v| v.to_string()),
        "payout_amount": d.payout_amount.map(|v| v.to_string()),
        "refund_amount": d.refund_amount.map(|v| v.to_string()),
        "fee_amount": d.fee_amount.map(|v| v.to_string()),
    })
}

impl Engine {
    /// Решение сотрудника по заявке на ручном разборе. Отказ ([`Refusal`]) ничего не меняет.
    pub async fn operator_decide(
        &self,
        req: OperatorRequest<'_>,
    ) -> Result<OrderRow, OperatorError> {
        let staff = req.staff;
        if !staff.active() {
            return Err(Refusal::Forbidden(req.decision.min_role()).into());
        }
        if staff.role < req.decision.min_role() {
            return Err(Refusal::Forbidden(req.decision.min_role()).into());
        }
        let reason = req.reason.trim();
        if reason.chars().count() < 3 {
            return Err(Refusal::ReasonTooShort.into());
        }
        let mut tx = self.pool.begin().await?;
        let order = storage::orders::get(&mut tx, req.order_id, true).await?;
        if order.state != OrderState::ManualReview {
            return Err(Refusal::NotInReview(order.state).into());
        }
        let ops = storage::operations::for_order(&mut tx, order.id).await?;
        if let Some(op) = ops
            .iter()
            .find(|o| matches!(o.status, OpStatus::Pending | OpStatus::Dispatched))
        {
            return Err(Refusal::InFlight(op.idempotency_key.clone()).into());
        }
        let d = storage::orders::details(&mut tx, order.id).await?;
        let before = snapshot(&order, &d);
        let received = storage::ledger::order_has(&mut tx, order.id, TxKind::Intake).await?;
        let flow = order.direction.flow();
        let asset_in = d.intake_asset.unwrap_or(order.asset);
        let ctx = EffectCtx {
            actor: Some(Actor::Staff(staff.tg_user_id)),
            ..EffectCtx::default()
        };
        let staff_id = staff.tg_user_id;
        // Активация без ответа держит кошелёк на паузе, а чат — занятым (аудит 11.11.2026,
        // Ю-2). Решение, которое закрывает её исход, снимает и то и другое.
        let unanswered_intake = ops
            .iter()
            .find(|o| o.kind == OpKind::ActivateCheck && o.status == OpStatus::Unknown)
            .map(|o| o.platform)
            .filter(|_| {
                matches!(
                    req.decision,
                    OperatorDecision::ConfirmIntake { .. } | OperatorDecision::CloseWithoutPayout
                )
            });

        let updated = match &req.decision {
            OperatorDecision::ConfirmIntake { amount } => {
                if received {
                    return Err(Refusal::IntakeAlreadyPosted.into());
                }
                if *amount <= Decimal::ZERO || amount.scale() > 18 {
                    return Err(Refusal::BadAmount.into());
                }
                let op = ops
                    .iter()
                    .rev()
                    .find(|o| o.kind == OpKind::ActivateCheck && o.status == OpStatus::Unknown)
                    .ok_or(Refusal::NoIntakeOperation)?;
                storage::operations::resolve_by_staff(
                    &mut tx,
                    op.id,
                    OpStatus::Succeeded,
                    &json!({ "resolved_by": format!("staff:{staff_id}"), "received": amount.to_string(), "reason": reason }),
                    None,
                )
                .await?;
                let created_by = format!("staff:{staff_id}");
                storage::ledger::post(
                    &mut tx,
                    &ledger::intake(order.direction.source(), order.asset, *amount)?,
                    storage::ledger::PostingRefs {
                        order_id: Some(order.id),
                        operation_id: Some(op.id),
                        memo: Some(reason),
                        created_by: Some(&created_by),
                        ..Default::default()
                    },
                )
                .await?;
                let total = d.intake_amount.unwrap_or_default() + *amount;
                storage::orders::set_intake(&mut tx, &order, total, order.asset).await?
            }
            OperatorDecision::RetryPayout { previous_failed } => {
                if flow != Flow::CheckExchange {
                    return Err(Refusal::WrongFlow.into());
                }
                if !received {
                    return Err(Refusal::FundsNotReceived.into());
                }
                if let Some(op) = settled_by(&ops) {
                    return Err(Refusal::AlreadySettled(op.idempotency_key.clone()).into());
                }
                retire_unknown(&mut tx, &ops, *previous_failed, staff_id, reason).await?;
                let intake = d.intake_amount.ok_or(Refusal::FundsNotReceived)?;
                // Выплата по правилам: прежняя сумма, если уже считали; иначе — как движок.
                let patch = match d.payout_amount {
                    Some(payout) => {
                        check_limit(staff, payout)?;
                        ensure_target_hold(&mut tx, &order, order.asset, payout).await?;
                        OrderPatch::default()
                    }
                    None => {
                        ensure_target_hold(&mut tx, &order, order.asset, intake).await?;
                        let money = Money::new(intake, asset_in)?;
                        match decide(&mut tx, &order, &money).await? {
                            Rule::Payout {
                                fee_pct,
                                fee,
                                payout,
                            } => {
                                check_limit(staff, payout)?;
                                OrderPatch {
                                    fee_pct_applied: Some(fee_pct),
                                    fee_amount: Some(fee),
                                    payout_amount: Some(payout),
                                    ..OrderPatch::default()
                                }
                            }
                            Rule::Refund { code, .. } => {
                                return Err(Refusal::Rules(code.to_owned()).into());
                            }
                            Rule::Manual { reason } => {
                                return Err(Refusal::Rules(reason).into());
                            }
                        }
                    }
                };
                transit(
                    &mut tx,
                    &order,
                    Event::Operator(OperatorAction::RetryPayout),
                    &patch,
                    Some(reason),
                    ctx,
                )
                .await?
                .0
            }
            OperatorDecision::RetryInvoicePayment { previous_failed } => {
                if flow != Flow::InvoicePayment {
                    return Err(Refusal::WrongFlow.into());
                }
                if !received {
                    return Err(Refusal::FundsNotReceived.into());
                }
                let intake = d.intake_amount.ok_or(Refusal::FundsNotReceived)?;
                if d.quote_amount_in.is_some_and(|x| intake < x) {
                    return Err(Refusal::Underfunded.into());
                }
                if let Some(op) = settled_by(&ops) {
                    return Err(Refusal::AlreadySettled(op.idempotency_key.clone()).into());
                }
                retire_unknown(&mut tx, &ops, *previous_failed, staff_id, reason).await?;
                let amount = d.invoice_amount.ok_or(Refusal::WrongFlow)?;
                check_limit(staff, amount)?;
                ensure_target_hold(&mut tx, &order, order.asset, amount).await?;
                transit(
                    &mut tx,
                    &order,
                    Event::Operator(OperatorAction::RetryInvoicePayment),
                    &OrderPatch::default(),
                    Some(reason),
                    ctx,
                )
                .await?
                .0
            }
            OperatorDecision::Refund { previous_failed } => {
                if !received {
                    return Err(Refusal::FundsNotReceived.into());
                }
                if let Some(op) = settled_by(&ops) {
                    return Err(Refusal::AlreadySettled(op.idempotency_key.clone()).into());
                }
                retire_unknown(&mut tx, &ops, *previous_failed, staff_id, reason).await?;
                let intake = d.intake_amount.ok_or(Refusal::FundsNotReceived)?;
                let money = Money::new(intake, asset_in)?;
                let amount = match refund(&mut tx, &order, &money, "operator_refund").await? {
                    Rule::Refund { amount, .. } => amount,
                    Rule::Manual { reason } => return Err(Refusal::Rules(reason).into()),
                    Rule::Payout { .. } => return Err(Refusal::WrongFlow.into()),
                };
                check_limit(staff, amount)?;
                let patch = OrderPatch {
                    refund_amount: Some(amount),
                    failure_code: Some("operator_refund"),
                    ..OrderPatch::default()
                };
                transit(
                    &mut tx,
                    &order,
                    Event::Operator(OperatorAction::Refund),
                    &patch,
                    Some(reason),
                    ctx,
                )
                .await?
                .0
            }
            OperatorDecision::CloseAsPaid { evidence } => {
                let evidence = evidence.trim();
                if evidence.chars().count() < 5 {
                    return Err(Refusal::EvidenceRequired.into());
                }
                if !received {
                    return Err(Refusal::FundsNotReceived.into());
                }
                if let Some(op) = settled_by(&ops) {
                    return Err(Refusal::AlreadySettled(op.idempotency_key.clone()).into());
                }
                let op = ops
                    .iter()
                    .rev()
                    .find(|o| {
                        o.role == MoneyRole::Settle
                            && matches!(o.kind, OpKind::CreatePayoutCheck | OpKind::PayInvoice)
                            && matches!(o.status, OpStatus::Unknown | OpStatus::Failed)
                    })
                    .ok_or(Refusal::NoSettleAttempt)?;
                let amount = op.amount.unwrap_or_default();
                check_limit(staff, amount)?;
                let check = check_param_from(evidence, &order);
                storage::operations::resolve_by_staff(
                    &mut tx,
                    op.id,
                    OpStatus::Succeeded,
                    &json!({ "resolved_by": format!("staff:{staff_id}"), "verdict": "paid", "evidence": evidence }),
                    check.as_deref(),
                )
                .await?;
                let url = check.as_ref().map(|_| evidence);
                let patch = OrderPatch {
                    payout_check_url: url.filter(|_| flow == Flow::CheckExchange),
                    ..OrderPatch::default()
                };
                transit(
                    &mut tx,
                    &order,
                    Event::Operator(OperatorAction::CloseAsPaid),
                    &patch,
                    Some(reason),
                    EffectCtx {
                        cause_op: Some(op.id),
                        ..ctx
                    },
                )
                .await?
                .0
            }
            OperatorDecision::CloseWithoutPayout => {
                if let Some(op) = settled_by(&ops) {
                    return Err(Refusal::AlreadySettled(op.idempotency_key.clone()).into());
                }
                // Владелец закрывает: всё неясное считается несостоявшимся.
                for op in ops.iter().filter(|o| o.status == OpStatus::Unknown) {
                    storage::operations::resolve_by_staff(
                        &mut tx,
                        op.id,
                        OpStatus::Failed,
                        &json!({ "resolved_by": format!("staff:{staff_id}"), "verdict": "closed_without_payout", "reason": reason }),
                        None,
                    )
                    .await?;
                }
                transit(
                    &mut tx,
                    &order,
                    Event::Operator(OperatorAction::CloseWithoutPayout),
                    &OrderPatch::default(),
                    Some(reason),
                    ctx,
                )
                .await?
                .0
            }
        };

        let after_details = storage::orders::details(&mut tx, order.id).await?;
        let mut after = snapshot(&updated, &after_details);
        if let (OperatorDecision::CloseAsPaid { evidence }, Value::Object(map)) =
            (&req.decision, &mut after)
        {
            map.insert("evidence".to_owned(), json!(evidence));
        }
        let order_id = order.id.to_string();
        storage::audit::record(
            &mut tx,
            &NewAudit {
                staff_id,
                action: req.decision.audit_action(),
                target_type: Some("order"),
                target_id: Some(&order_id),
                before: Some(before),
                after: Some(after),
                reason: Some(reason),
                double_confirmed: req.double_confirmed,
            },
        )
        .await?;
        if let Some(platform) = unanswered_intake {
            storage::directions::clear_auto_pause_reason(
                &mut tx,
                platform,
                crate::UNANSWERED_PAUSE,
            )
            .await?;
        }
        tx.commit().await?;
        if let Some(platform) = unanswered_intake {
            self.unanswered_resolved(platform).await;
        }
        tracing::info!(
            order = order.id,
            staff = staff_id,
            action = req.decision.audit_action(),
            to = updated.state.as_db_str(),
            "operator decision applied"
        );
        self.notify(&updated);
        Ok(updated)
    }
}
