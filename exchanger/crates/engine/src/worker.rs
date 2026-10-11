//! Исполнитель outbox: операция → действие юзербота → итог + переход заявки (одна транзакция).
//!
//! | Итог юзербота | Операция | Событие заявки |
//! |---|---|---|
//! | `Err(BotError)` — не выполнялось | назад в `pending`, попытка не считается | — |
//! | активация `Received(m)` | `succeeded` | `CheckActivated` → решение ([`crate::settle`]) |
//! | активация `Rejected` | `failed` | `CheckRejected` |
//! | активация `Unknown` | повтор тем же `random_id`, пока есть попытки; затем `unknown` | `IntakeOutcomeUnknown` → `MANUAL_REVIEW` |
//! | чек `Issued` | `succeeded` | `PayoutConfirmed` / `RefundConfirmed` |
//! | чек `InsufficientFunds` | `failed` | `PayoutFailed` / `RefundFailed` + автопауза |
//! | чек `Unknown` | `unknown` → сверка `find_issued_check` | нашли — `…Confirmed`; нет — `…Failed` (человек) |
//! | счёт `Paid` | `succeeded` | `InvoicePaid` |
//! | счёт `Expired` | `failed` | `InvoiceUnpayable` → возврат |
//! | счёт остальное | `failed` / `unknown` | `InvoicePayNeedsHuman` |
//!
//! Выплаты и оплаты не повторяются никогда: операция `unknown` держит уникальный индекс
//! `operations_one_settle_per_order`, и новую выплату по заявке создаст только оператор.

use chrono::Utc;
use domain::Money;
use domain::fsm::{Event, OpKind, OrderState, Settlement};
use serde_json::json;
use sqlx::PgConnection;
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

impl Engine {
    /// Забрать созревшие операции и выполнить их по очереди. Возвращает число взятых.
    pub async fn process_due(&self) -> Result<usize, EngineError> {
        let ops = {
            let mut conn = self.pool.acquire().await?;
            storage::operations::claim_due(&mut conn, self.cfg.batch).await?
        };
        for op in &ops {
            if let Err(e) = self.execute(op).await {
                tracing::error!(op = op.id, key = %op.idempotency_key, error = %e, "operation handling failed");
            }
        }
        Ok(ops.len())
    }

    async fn execute(&self, op: &OperationRow) -> Result<(), EngineError> {
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

    /// Действие не выполнялось: вернуть операцию в очередь.
    async fn release(&self, op: &OperationRow, e: &BotError) -> Result<(), EngineError> {
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
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(e) => return self.release(op, &e).await,
        };
        if let ActivationOutcome::Unknown { raw } = &outcome {
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
                let text = raw.map(|m| m.text);
                storage::operations::finish(
                    &mut tx,
                    op.id,
                    OpStatus::Unknown,
                    &json!({ "reply": text }),
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
                let reason = "activation outcome unknown: check the wallet chat and balance";
                let patch = OrderPatch {
                    manual_reason: Some(reason),
                    ..OrderPatch::default()
                };
                transit(
                    &mut tx,
                    &order,
                    Event::ReconciliationAmbiguous,
                    &patch,
                    Some(reason),
                    cause,
                )
                .await?
                .0
            }
        };
        tx.commit().await?;
        self.notify(&order);
        Ok(())
    }

    /// После зачисления: выплата, возврат или человек — в той же транзакции.
    async fn settle(
        &self,
        conn: &mut PgConnection,
        order: &OrderRow,
        received: &Money,
    ) -> Result<OrderRow, EngineError> {
        let decision = decide(&mut *conn, order, received).await?;
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
            Decision::Refund { code } => (
                Event::Settle(Settlement::Refund),
                OrderPatch {
                    refund_amount: Some(received.amount()),
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
            storage::directions::auto_pause(&mut tx, order.direction, reason).await?;
            tracing::error!(order = order.id, direction = order.direction.code(), %reason, "direction auto-paused");
        }
        tx.commit().await?;
        self.notify(&order);
        Ok(())
    }

    /// Сверка выплаты с неизвестным исходом. Ошибка юзербота — оставить как есть: следующий
    /// проход [`Engine::resolve_unknown`] попробует снова.
    async fn reconcile_issue(&self, op: &OperationRow, money: &Money) -> Result<(), EngineError> {
        let wallet = self.wallet(op.platform)?.clone();
        match wallet.find_issued_check(money, &tag(op)).await {
            Ok(Some(check)) => self.confirm_issue(op, &check, "reconciled").await,
            Ok(None) => {
                self.fail_issue(
                    op,
                    OpStatus::Unknown,
                    "check issue outcome unknown and no check found: verify in the wallet",
                    false,
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
            storage::directions::auto_pause(
                &mut tx,
                order.direction,
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
            let money = op_money(&op)?;
            self.reconcile_issue(&op, &money).await?;
        }
        Ok(())
    }

    /// После рестарта: операции, взятые в работу до падения.
    ///
    /// - активация — снова в очередь (повтор с тем же `random_id` безопасен);
    /// - чек выплаты или возврата — исход неизвестен: только сверка;
    /// - оплата счёта — к человеку (повторное нажатие «Оплатить» запрещено).
    pub async fn recover(&self) -> Result<(), EngineError> {
        let dispatched: Vec<OperationRow> = {
            let mut conn = self.pool.acquire().await?;
            storage::operations::in_flight(&mut conn)
                .await?
                .into_iter()
                .filter(|op| op.status == OpStatus::Dispatched)
                .collect()
        };
        for op in dispatched {
            tracing::warn!(op = op.id, key = %op.idempotency_key, kind = op.kind.as_db_str(), "recovering operation interrupted by restart");
            match op.kind {
                OpKind::ActivateCheck => {
                    let retried = {
                        let mut conn = self.pool.acquire().await?;
                        storage::operations::retry(&mut conn, op.id, Utc::now(), "restart").await?
                    };
                    if !retried {
                        self.on_activation(&op, Ok(ActivationOutcome::Unknown { raw: None }))
                            .await?;
                    }
                }
                OpKind::CreatePayoutCheck | OpKind::CreateRefundCheck => {
                    let money = op_money(&op)?;
                    self.on_issue(
                        &op,
                        &money,
                        Ok(IssueOutcome::Unknown {
                            detail: "interrupted by restart".to_owned(),
                        }),
                    )
                    .await?;
                }
                OpKind::PayInvoice => {
                    self.on_payment(
                        &op,
                        Ok(PayOutcome::Unknown {
                            detail: "interrupted by restart".to_owned(),
                        }),
                    )
                    .await?;
                }
            }
        }
        self.resolve_unknown().await
    }
}
