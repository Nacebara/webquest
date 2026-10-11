//! Исполнитель outbox: операция → действие юзербота → итог + переход заявки (одна транзакция).
//!
//! | Итог юзербота | Операция | Событие заявки |
//! |---|---|---|
//! | `Err(BotError)` — не выполнялось | назад в `pending`, попытка не считается; долго (`escalate_after`) — к человеку и пауза | — |
//! | `AccountRestricted` / `NotAuthorized` | к человеку сразу, направления платформы на паузу | → `MANUAL_REVIEW` |
//! | активация `Received(m)` | `succeeded` | `CheckActivated` → решение ([`crate::settle`]) |
//! | активация `Rejected` | `failed` | `CheckRejected`; на повторной попытке «уже активирован» / «не найден» — к человеку |
//! | активация `Unknown` | повтор тем же `random_id`, пока есть попытки; затем `unknown` | `IntakeOutcomeUnknown` → `MANUAL_REVIEW`; незнакомый текст — пауза |
//! | чек `Issued` | `succeeded` | `PayoutConfirmed` / `RefundConfirmed` |
//! | чек `InsufficientFunds` | `failed` | `PayoutFailed` / `RefundFailed` + пауза выплат с этого кошелька |
//! | чек `Unknown` | `unknown` → сверка `find_issued_check` | нашли — `…Confirmed`; нет дольше `reconcile_grace` — `…Failed` (человек) |
//! | счёт `Paid` | `succeeded` | `InvoicePaid` |
//! | счёт `Expired` | `failed` | `InvoiceUnpayable` → возврат |
//! | счёт остальное | `failed` / `unknown` | `InvoicePayNeedsHuman` |
//! | ошибка движка после действия | `unknown` | к человеку ([`Engine::park`]) |
//!
//! Выплаты и оплаты не повторяются никогда: операция `unknown` держит уникальный индекс
//! `operations_one_settle_per_order`, и новую выплату по заявке создаст только оператор.
//! Один созданный чек не закроет две операции (`operations_check_issued_once`).

use chrono::Utc;
use domain::Money;
use domain::fsm::{Event, OpKind, OrderState, Settlement};
use serde_json::json;
use sqlx::PgConnection;
use storage::StorageError;
use storage::directions::Side;
use storage::operations::{OpStatus, OperationRow};
use storage::orders::{OrderPatch, OrderRow};
use userbot::{
    ActivationOutcome, BotError, IssueOutcome, IssuedCheck, OpTag, PayOutcome, RejectReason,
};

use crate::effects::{EffectCtx, transit};
use crate::settle::{Decision, decide};
use crate::{Engine, EngineError};

fn tag(op: &OperationRow) -> OpTag {
    OpTag {
        key: op.idempotency_key.clone(),
        random_id: op.random_id,
    }
}

fn reject_code(reason: RejectReason) -> &'static str {
    match reason {
        RejectReason::AlreadyActivated => "check_already_activated",
        RejectReason::NotFound => "check_not_found",
        RejectReason::NotForUs => "check_not_for_us",
        RejectReason::NeedsSubscription => "check_needs_subscription",
        RejectReason::Captcha => "check_captcha",
        RejectReason::PasswordProtected => "check_password",
        RejectReason::PremiumOnly => "check_premium_only",
    }
}

fn op_money(op: &OperationRow) -> Result<Money, EngineError> {
    match (op.amount, op.asset) {
        (Some(amount), Some(asset)) => Ok(Money::new(amount, asset)?),
        _ => Err(EngineError::Invariant(format!(
            "operation {} has no amount",
            op.id
        ))),
    }
}

fn order_id(op: &OperationRow) -> Result<i64, EngineError> {
    op.order_id
        .ok_or_else(|| EngineError::Invariant(format!("operation {} has no order", op.id)))
}

fn is_money_out(kind: OpKind) -> bool {
    matches!(
        kind,
        OpKind::CreatePayoutCheck | OpKind::CreateRefundCheck | OpKind::PayInvoice
    )
}

fn older_than(op: &OperationRow, age: std::time::Duration) -> bool {
    let age = chrono::Duration::from_std(age).unwrap_or_default();
    op.dispatched_at.is_some_and(|t| Utc::now() - t >= age)
}

impl Engine {
    /// Брать созревшие операции по одной — непосредственно перед действием — и выполнять.
    /// Не больше `batch` за вызов. Возвращает число взятых.
    pub async fn process_due(&self) -> Result<usize, EngineError> {
        let max = usize::try_from(self.cfg.batch.max(1)).unwrap_or(1);
        let mut done = 0;
        while done < max {
            let op = {
                let mut conn = self.pool.acquire().await?;
                storage::operations::claim_due(&mut conn, 1).await?.pop()
            };
            let Some(op) = op else { break };
            done += 1;
            if let Err(e) = self.execute(&op).await {
                tracing::error!(op = op.id, key = %op.idempotency_key, error = %e, "operation handling failed");
                self.park(&op, &format!("engine error: {e}")).await;
            }
        }
        Ok(done)
    }

    async fn execute(&self, op: &OperationRow) -> Result<(), EngineError> {
        // Операция принадлежит кошельку (аккаунту), который её начал. После переключения на
        // запасной аккаунт ни активацию, ни сверку на чужом аккаунте делать нельзя.
        let active = {
            let mut conn = self.pool.acquire().await?;
            storage::wallets::active(&mut conn, op.platform)
                .await?
                .map(|(id, _)| id)
        };
        if op.wallet_account_id.is_some() && active != op.wallet_account_id {
            self.park(
                op,
                &format!(
                    "operation belongs to wallet account {:?}, active is {:?}",
                    op.wallet_account_id, active
                ),
            )
            .await;
            return Ok(());
        }
        if is_money_out(op.kind) {
            let mut conn = self.pool.acquire().await?;
            if storage::settings::flag_on(&mut conn, "kill_switch_payouts").await? {
                let at = Utc::now()
                    + chrono::Duration::from_std(self.cfg.unavailable_backoff).unwrap_or_default();
                storage::operations::postpone(
                    &mut conn,
                    op.id,
                    at,
                    "kill switch: payouts are stopped",
                )
                .await?;
                return Ok(());
            }
        }

        let wallet = self.wallet(op.platform)?.clone();
        let tag = tag(op);
        match op.kind {
            OpKind::ActivateCheck => {
                let param = op.start_param.as_deref().ok_or_else(|| {
                    EngineError::Invariant(format!("operation {} has no check", op.id))
                })?;
                let result = wallet.activate_check(param, &tag).await;
                self.on_activation(op, result).await
            }
            OpKind::CreatePayoutCheck | OpKind::CreateRefundCheck => {
                let money = op_money(op)?;
                let result = wallet.issue_check(&money, &tag).await;
                self.on_issue(op, &money, result).await
            }
            OpKind::PayInvoice => {
                let money = op_money(op)?;
                let param = op.start_param.as_deref().ok_or_else(|| {
                    EngineError::Invariant(format!("operation {} has no invoice", op.id))
                })?;
                let result = wallet.pay_invoice(param, money.asset(), &money, &tag).await;
                self.on_payment(op, result).await
            }
        }
    }

    /// Действие не выполнялось: вернуть операцию в очередь. Аккаунт ограничен или
    /// разлогинен, или операция не выполняется дольше `escalate_after` — к человеку и пауза.
    async fn release(&self, op: &OperationRow, e: &BotError) -> Result<(), EngineError> {
        let fatal = matches!(e, BotError::AccountRestricted | BotError::NotAuthorized);
        let stuck = op.first_released_at.is_some_and(|t| {
            Utc::now() - t
                >= chrono::Duration::from_std(self.cfg.escalate_after).unwrap_or_default()
        });
        if fatal || stuck {
            let reason = if fatal {
                format!("wallet account cannot act: {e}")
            } else {
                format!("wallet action not performed for too long: {e}")
            };
            {
                let mut conn = self.pool.acquire().await?;
                for side in [Side::Source, Side::Target] {
                    storage::directions::auto_pause_platform(&mut conn, op.platform, side, &reason)
                        .await?;
                }
            }
            tracing::error!(op = op.id, platform = op.platform.db_name(), %reason, "directions of the platform auto-paused");
            self.park(op, &reason).await;
            return Ok(());
        }
        let wait = match e {
            BotError::FloodWait(d) => *d,
            _ => self.cfg.unavailable_backoff,
        };
        let at = Utc::now() + chrono::Duration::from_std(wait).unwrap_or_default();
        tracing::warn!(op = op.id, error = %e, "wallet action not performed, rescheduled");
        let mut conn = self.pool.acquire().await?;
        storage::operations::release(&mut conn, op.id, at, &e.to_string()).await?;
        Ok(())
    }

    /// Отдать заявку человеку, когда автоматика не может довести операцию: операция —
    /// `unknown` (держит индекс расчёта), заявка — в `MANUAL_REVIEW` с причиной. Без
    /// проводок: что произошло с деньгами, решит оператор. Ошибку не возвращает — если и это
    /// не удалось, операция останется `dispatched`, и её подберёт проход «зависших».
    pub(crate) async fn park(&self, op: &OperationRow, reason: &str) {
        if let Err(e) = self.try_park(op, reason).await {
            tracing::error!(op = op.id, error = %e, %reason, "cannot hand the operation to a human; it stays for the stale sweep");
        }
    }

    async fn try_park(&self, op: &OperationRow, reason: &str) -> Result<(), EngineError> {
        let mut tx = self.pool.begin().await?;
        match storage::operations::finish(
            &mut tx,
            op.id,
            OpStatus::Unknown,
            &json!({ "parked": reason }),
            None,
            Some(reason),
        )
        .await
        {
            Ok(()) | Err(StorageError::OperationState(_)) => {}
            Err(e) => return Err(e.into()),
        }
        let Some(id) = op.order_id else {
            tx.commit().await?;
            return Ok(());
        };
        let mut order = storage::orders::get(&mut tx, id, true).await?;
        let events: &[Event] = match order.state {
            OrderState::IntakePending => {
                &[Event::IntakeOutcomeUnknown, Event::ReconciliationAmbiguous]
            }
            OrderState::IntakeUnknown => &[Event::ReconciliationAmbiguous],
            OrderState::PayoutPending => &[Event::PayoutFailed],
            OrderState::RefundPending => &[Event::RefundFailed],
            OrderState::InvoicePayPending => &[Event::InvoicePayNeedsHuman],
            _ => &[],
        };
        let patch = OrderPatch {
            manual_reason: Some(reason),
            ..OrderPatch::default()
        };
        for event in events {
            order = transit(
                &mut tx,
                &order,
                *event,
                &patch,
                Some(reason),
                EffectCtx::default(),
            )
            .await?
            .0;
        }
        tx.commit().await?;
        tracing::error!(op = op.id, order = order.id, %reason, "order handed to a human");
        self.notify(&order);
        Ok(())
    }

    async fn locked_order(
        &self,
        conn: &mut PgConnection,
        op: &OperationRow,
    ) -> Result<OrderRow, EngineError> {
        Ok(storage::orders::get(conn, order_id(op)?, true).await?)
    }

    async fn on_activation(
        &self,
        op: &OperationRow,
        result: Result<ActivationOutcome, BotError>,
    ) -> Result<(), EngineError> {
        let mut outcome = match result {
            Ok(outcome) => outcome,
            Err(e) => return self.release(op, &e).await,
        };
        // Повторная попытка: «уже активирован» или «не найден» может означать, что чек забрала
        // НАША прошлая попытка (lovec AUDIT.md:46). Списывать деньги клиента нельзя — к человеку.
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
                tracing::info!(
                    op = op.id,
                    attempt = op.attempts,
                    "activation outcome unknown, will ask again"
                );
                return Ok(());
            }
        }

        let mut tx = self.pool.begin().await?;
        let order = self.locked_order(&mut tx, op).await?;
        if order.state != OrderState::IntakePending {
            return Err(EngineError::Invariant(format!(
                "activation result for order {} in {:?}",
                order.id, order.state
            )));
        }
        let cause = EffectCtx {
            cause_op: Some(op.id),
            ..EffectCtx::default()
        };
        let order = match outcome {
            ActivationOutcome::Received(m) => {
                storage::operations::finish(
                    &mut tx,
                    op.id,
                    OpStatus::Succeeded,
                    &json!({ "received": m.amount().to_string(), "asset": m.asset().code() }),
                    None,
                    None,
                )
                .await?;
                let patch = OrderPatch {
                    intake_amount: Some(m.amount()),
                    intake_asset: Some(m.asset()),
                    ..OrderPatch::default()
                };
                let (order, _) = transit(
                    &mut tx,
                    &order,
                    Event::CheckActivated { fully_funded: true },
                    &patch,
                    None,
                    EffectCtx {
                        intake_asset: Some(m.asset()),
                        ..cause
                    },
                )
                .await?;
                self.settle(&mut tx, &order, &m).await?
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
                let patch = OrderPatch {
                    failure_code: Some(code),
                    ..OrderPatch::default()
                };
                transit(
                    &mut tx,
                    &order,
                    Event::CheckRejected,
                    &patch,
                    Some(code),
                    cause,
                )
                .await?
                .0
            }
            ActivationOutcome::Unknown { raw } => {
                let unrecognised = raw.is_some();
                let text = raw.map(|m| m.text);
                storage::operations::finish(
                    &mut tx,
                    op.id,
                    OpStatus::Unknown,
                    &json!({ "reply": text, "repeated_loss": repeated_loss }),
                    None,
                    Some("activation outcome unknown"),
                )
                .await?;
                let (order, _) = transit(
                    &mut tx,
                    &order,
                    Event::IntakeOutcomeUnknown,
                    &OrderPatch::default(),
                    None,
                    cause,
                )
                .await?;
                let reason = if repeated_loss {
                    "a repeated activation says the check is gone: our earlier attempt may have won; check the wallet"
                } else {
                    "activation outcome unknown: check the wallet chat and balance"
                };
                let patch = OrderPatch {
                    manual_reason: Some(reason),
                    ..OrderPatch::default()
                };
                let (order, _) = transit(
                    &mut tx,
                    &order,
                    Event::ReconciliationAmbiguous,
                    &patch,
                    Some(reason),
                    cause,
                )
                .await?;
                // Незнакомый ответ кошелька — приём на этой платформе на паузу (CLAUDE.md п. 13).
                if unrecognised {
                    storage::directions::auto_pause_platform(
                        &mut tx,
                        op.platform,
                        Side::Source,
                        "unrecognised wallet reply to a check activation",
                    )
                    .await?;
                    tracing::error!(
                        op = op.id,
                        platform = op.platform.db_name(),
                        "unrecognised activation reply: intake paused"
                    );
                }
                order
            }
        };
        tx.commit().await?;
        self.notify(&order);
        Ok(())
    }

    /// После зачисления: выплата, возврат или человек — в той же транзакции. Ошибка расчёта
    /// не откатывает зачисление: заявка уходит человеку с причиной.
    async fn settle(
        &self,
        conn: &mut PgConnection,
        order: &OrderRow,
        received: &Money,
    ) -> Result<OrderRow, EngineError> {
        let decision = match decide(&mut *conn, order, received).await {
            Ok(d) => d,
            Err(e @ (EngineError::Storage(_) | EngineError::Db(_))) => return Err(e),
            Err(e) => Decision::Manual {
                reason: format!("settlement failed: {e}"),
            },
        };
        let (event, patch, reason) = match &decision {
            Decision::Payout {
                fee_pct,
                fee,
                payout,
            } => (
                Event::Settle(Settlement::Payout),
                OrderPatch {
                    fee_pct_applied: Some(*fee_pct),
                    fee_amount: Some(*fee),
                    payout_amount: Some(*payout),
                    ..OrderPatch::default()
                },
                None,
            ),
            Decision::Refund { code, amount } => (
                Event::Settle(Settlement::Refund),
                OrderPatch {
                    refund_amount: Some(*amount),
                    failure_code: Some(code),
                    ..OrderPatch::default()
                },
                Some(*code),
            ),
            Decision::Manual { reason } => (
                Event::Settle(Settlement::Manual),
                OrderPatch {
                    manual_reason: Some(reason),
                    ..OrderPatch::default()
                },
                Some(reason.as_str()),
            ),
        };
        let (order, _) = transit(
            &mut *conn,
            order,
            event,
            &patch,
            reason,
            EffectCtx::default(),
        )
        .await?;
        Ok(order)
    }

    async fn on_issue(
        &self,
        op: &OperationRow,
        money: &Money,
        result: Result<IssueOutcome, BotError>,
    ) -> Result<(), EngineError> {
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(e) => return self.release(op, &e).await,
        };
        match outcome {
            IssueOutcome::Issued { check, method } => {
                self.confirm_issue(op, &check, &format!("{method:?}")).await
            }
            IssueOutcome::InsufficientFunds => {
                self.fail_issue(
                    op,
                    OpStatus::Failed,
                    "wallet has not enough funds for the check",
                    true,
                )
                .await
            }
            IssueOutcome::Unknown { detail } => {
                {
                    let mut conn = self.pool.acquire().await?;
                    storage::operations::finish(
                        &mut conn,
                        op.id,
                        OpStatus::Unknown,
                        &json!({ "detail": detail }),
                        None,
                        Some(&detail),
                    )
                    .await?;
                }
                tracing::warn!(op = op.id, %detail, "check issue outcome unknown, reconciling");
                self.reconcile_issue(op, money).await
            }
        }
    }

    /// Чек создан: итог операции, переход и проводка.
    async fn confirm_issue(
        &self,
        op: &OperationRow,
        check: &IssuedCheck,
        method: &str,
    ) -> Result<(), EngineError> {
        let event = match op.kind {
            OpKind::CreateRefundCheck => Event::RefundConfirmed,
            _ => Event::PayoutConfirmed,
        };
        let mut tx = self.pool.begin().await?;
        let order = self.locked_order(&mut tx, op).await?;
        storage::operations::finish(
            &mut tx,
            op.id,
            OpStatus::Succeeded,
            &json!({ "url": check.url, "method": method }),
            Some(&check.param),
            None,
        )
        .await?;
        let patch = OrderPatch {
            payout_check_url: Some(&check.url),
            ..OrderPatch::default()
        };
        let (order, _) = transit(
            &mut tx,
            &order,
            event,
            &patch,
            None,
            EffectCtx {
                cause_op: Some(op.id),
                ..EffectCtx::default()
            },
        )
        .await?;
        tx.commit().await?;
        self.notify(&order);
        Ok(())
    }

    /// Чек не создан или исход не доказан: заявка к человеку. `status` — итог операции
    /// (`unknown` оставляет её блокировать новую выплату, пока оператор не разберётся).
    /// `pause` — выплаты с этого кошелька на паузу.
    async fn fail_issue(
        &self,
        op: &OperationRow,
        status: OpStatus,
        reason: &str,
        pause: bool,
    ) -> Result<(), EngineError> {
        let event = match op.kind {
            OpKind::CreateRefundCheck => Event::RefundFailed,
            _ => Event::PayoutFailed,
        };
        let mut tx = self.pool.begin().await?;
        let order = self.locked_order(&mut tx, op).await?;
        storage::operations::finish(
            &mut tx,
            op.id,
            status,
            &json!({ "reason": reason }),
            None,
            Some(reason),
        )
        .await?;
        let patch = OrderPatch {
            manual_reason: Some(reason),
            ..OrderPatch::default()
        };
        let (order, _) = transit(
            &mut tx,
            &order,
            event,
            &patch,
            Some(reason),
            EffectCtx::default(),
        )
        .await?;
        if pause {
            // Короток кошелёк, с которого платили (`op.platform`): на паузу все направления,
            // которые выплачивают с него, — и для выплаты, и для возврата.
            storage::directions::auto_pause_platform(&mut tx, op.platform, Side::Target, reason)
                .await?;
            tracing::error!(order = order.id, platform = op.platform.db_name(), %reason, "payouts from the wallet auto-paused");
        }
        tx.commit().await?;
        self.notify(&order);
        Ok(())
    }

    /// Сверка выплаты с неизвестным исходом. Чек не найден, но с начала операции прошло
    /// меньше `reconcile_grace` — ждём (бот мог ещё не дописать кнопку или ответ). Ошибка
    /// юзербота — следующий проход [`Engine::resolve_unknown`] попробует снова; дольше
    /// `escalate_after` — к человеку.
    async fn reconcile_issue(&self, op: &OperationRow, money: &Money) -> Result<(), EngineError> {
        let wallet = self.wallet(op.platform)?.clone();
        match wallet.find_issued_check(money, &tag(op)).await {
            Ok(Some(check)) => self.confirm_issue(op, &check, "reconciled").await,
            Ok(None) if !older_than(op, self.cfg.reconcile_grace) => {
                tracing::info!(op = op.id, "no check found yet, reconciliation continues");
                Ok(())
            }
            Ok(None) => {
                self.fail_issue(
                    op,
                    OpStatus::Unknown,
                    "check issue outcome unknown and no check found: verify in the wallet",
                    true,
                )
                .await
            }
            Err(e) if older_than(op, self.cfg.escalate_after) => {
                self.fail_issue(
                    op,
                    OpStatus::Unknown,
                    &format!("check issue outcome unknown, reconciliation impossible: {e}"),
                    true,
                )
                .await
            }
            Err(e) => {
                tracing::warn!(op = op.id, error = %e, "reconciliation postponed");
                Ok(())
            }
        }
    }

    async fn on_payment(
        &self,
        op: &OperationRow,
        result: Result<PayOutcome, BotError>,
    ) -> Result<(), EngineError> {
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(e) => return self.release(op, &e).await,
        };
        let (status, event, reason): (OpStatus, Event, Option<String>) = match &outcome {
            PayOutcome::Paid => (OpStatus::Succeeded, Event::InvoicePaid, None),
            PayOutcome::Expired => (
                OpStatus::Failed,
                Event::InvoiceUnpayable,
                Some("invoice expired".to_owned()),
            ),
            PayOutcome::AlreadyPaid => (
                OpStatus::Failed,
                Event::InvoicePayNeedsHuman,
                Some("invoice is already paid: find out by whom".to_owned()),
            ),
            PayOutcome::InsufficientFunds => (
                OpStatus::Failed,
                Event::InvoicePayNeedsHuman,
                Some("wallet has not enough funds for the invoice".to_owned()),
            ),
            PayOutcome::NeedsHuman { reason } => (
                OpStatus::Failed,
                Event::InvoicePayNeedsHuman,
                Some(format!("pay manually: {reason}")),
            ),
            PayOutcome::Unknown { detail } => (
                OpStatus::Unknown,
                Event::InvoicePayNeedsHuman,
                Some(format!("payment outcome unknown: {detail}")),
            ),
        };
        let mut tx = self.pool.begin().await?;
        let order = self.locked_order(&mut tx, op).await?;
        storage::operations::finish(
            &mut tx,
            op.id,
            status,
            &json!({ "outcome": format!("{outcome:?}") }),
            None,
            reason.as_deref(),
        )
        .await?;
        let refund_amount = storage::orders::details(&mut tx, order.id)
            .await?
            .intake_amount;
        let patch = OrderPatch {
            manual_reason: reason
                .as_deref()
                .filter(|_| event == Event::InvoicePayNeedsHuman),
            refund_amount: refund_amount.filter(|_| event == Event::InvoiceUnpayable),
            ..OrderPatch::default()
        };
        let (order, _) = transit(
            &mut tx,
            &order,
            event,
            &patch,
            reason.as_deref(),
            EffectCtx {
                cause_op: Some(op.id),
                ..EffectCtx::default()
            },
        )
        .await?;
        if outcome == PayOutcome::InsufficientFunds {
            storage::directions::auto_pause_platform(
                &mut tx,
                op.platform,
                Side::Target,
                "invoice wallet has not enough funds",
            )
            .await?;
        }
        tx.commit().await?;
        self.notify(&order);
        Ok(())
    }

    /// Выплаты с неизвестным исходом, по которым заявка ещё ждёт: снова сверка.
    pub async fn resolve_unknown(&self) -> Result<(), EngineError> {
        let pending: Vec<OperationRow> = {
            let mut conn = self.pool.acquire().await?;
            let mut out = Vec::new();
            for op in storage::operations::in_flight(&mut conn).await? {
                if op.status != OpStatus::Unknown
                    || !matches!(
                        op.kind,
                        OpKind::CreatePayoutCheck | OpKind::CreateRefundCheck
                    )
                {
                    continue;
                }
                let order = storage::orders::get(&mut conn, order_id(&op)?, false).await?;
                if matches!(
                    order.state,
                    OrderState::PayoutPending | OrderState::RefundPending
                ) {
                    out.push(op);
                }
            }
            out
        };
        for op in pending {
            let result = match op_money(&op) {
                Ok(money) => self.reconcile_issue(&op, &money).await,
                Err(e) => Err(e),
            };
            if let Err(e) = result {
                tracing::error!(op = op.id, error = %e, "reconciliation failed");
                self.park(&op, &format!("reconciliation failed: {e}")).await;
            }
        }
        Ok(())
    }

    /// После рестарта: операции, взятые в работу до падения.
    ///
    /// - активация — снова в очередь (повтор с тем же `random_id` безопасен);
    /// - чек выплаты или возврата — исход неизвестен: только сверка;
    /// - оплата счёта — к человеку (повторное нажатие «Оплатить» запрещено).
    ///
    /// Ошибка по одной операции не останавливает остальные.
    pub async fn recover(&self) -> Result<(), EngineError> {
        self.recover_dispatched(None).await?;
        self.resolve_unknown().await
    }

    /// Операции `dispatched` (все или только взятые раньше `min_age` — «зависшие» на ходу).
    pub(crate) async fn recover_dispatched(
        &self,
        min_age: Option<std::time::Duration>,
    ) -> Result<(), EngineError> {
        let dispatched: Vec<OperationRow> = {
            let mut conn = self.pool.acquire().await?;
            storage::operations::in_flight(&mut conn)
                .await?
                .into_iter()
                .filter(|op| op.status == OpStatus::Dispatched)
                .filter(|op| min_age.is_none_or(|age| older_than(op, age)))
                .collect()
        };
        for op in dispatched {
            tracing::warn!(op = op.id, key = %op.idempotency_key, kind = op.kind.as_db_str(), "recovering an interrupted operation");
            if let Err(e) = self.recover_one(&op).await {
                tracing::error!(op = op.id, error = %e, "recovery failed");
                self.park(&op, &format!("recovery failed: {e}")).await;
            }
        }
        Ok(())
    }

    async fn recover_one(&self, op: &OperationRow) -> Result<(), EngineError> {
        match op.kind {
            OpKind::ActivateCheck => {
                let retried = {
                    let mut conn = self.pool.acquire().await?;
                    storage::operations::retry(&mut conn, op.id, Utc::now(), "restart").await?
                };
                if !retried {
                    self.on_activation(op, Ok(ActivationOutcome::Unknown { raw: None }))
                        .await?;
                }
                Ok(())
            }
            OpKind::CreatePayoutCheck | OpKind::CreateRefundCheck => {
                let money = op_money(op)?;
                self.on_issue(
                    op,
                    &money,
                    Ok(IssueOutcome::Unknown {
                        detail: "interrupted by restart".to_owned(),
                    }),
                )
                .await
            }
            OpKind::PayInvoice => {
                self.on_payment(
                    op,
                    Ok(PayOutcome::Unknown {
                        detail: "interrupted by restart".to_owned(),
                    }),
                )
                .await
            }
        }
    }
}
