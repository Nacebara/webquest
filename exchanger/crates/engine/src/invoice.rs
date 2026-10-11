//! Оплата чужого счёта деньгами клиента с другой платформы (SPEC §4, П3–П7, П19).
//!
//! 1. [`Engine::submit_invoice`] — юзербот платформы счёта открывает счёт, ничего не платя;
//!    проверки (одноразовый, в USDT, активен, лимиты, резерв платформы счёта) → котировка `X`
//!    (`domain::pricing::quote_invoice_payment`) → `AWAITING_FUNDS` с удержанием суммы счёта
//!    `A`. Транзакция БД открывается только после просмотра счёта.
//! 2. Чек клиента на платформе входа, пока заявка ждёт, идёт в неё ([`Engine::fund_invoice`]):
//!    `ClientCheckSubmitted` → активация (как у обмена).
//! 3. Зачисление ([`Engine::invoice_received`]): пришло меньше `X` — снова ждём, сумма копится;
//!    хватает — `Settle(PayInvoice)` и операция оплаты (одна попытка).
//! 4. Счёт оплачен ([`Engine::settle_excess`]): переплата не меньше `min_refund_amount` — чек
//!    на излишек на платформе входа, меньше — в доход (`excess_kept`).
//! 5. Котировка истекла ([`Engine::expire_quotes`]) или клиент отменил
//!    ([`Engine::cancel_invoice`]): денег нет — `EXPIRED` / `CANCELLED`, часть пришла — возврат.
//!
//! Фиатные счета пока не принимаем: сценарий оплаты платит только ровно ту сумму, под которую
//! клиент прислал деньги, а у фиатного счёта сумма в USDT плавает (HANDOVER §8.1).

use chrono::Utc;
use domain::fsm::{Event, Flow, MoneyRole, OpKind, OrderState, Settlement};
use domain::pricing::{self, FeePolicy};
use domain::{Asset, Decimal, Direction, Money, Platform, Step, ledger};
use parsers::{InvoiceCard, InvoiceStatus, LinkKind, WalletLink};
use sqlx::PgConnection;
use storage::StorageError;
use storage::ledger::PostingRefs;
use storage::orders::{NewOrder, OrderPatch, OrderRow};

use crate::effects::{EffectCtx, create_op, transit};
use crate::settle::{Decision, fee_pct, refund};
use crate::submit::{SubmitReject, Submitted, daily_limit, setting_decimal};
use crate::{Engine, EngineError};

/// Срок котировки по умолчанию (`settings.quote_ttl_seconds`).
const DEFAULT_QUOTE_TTL_SECS: i64 = 900;

/// Что не так со счётом. Денег клиента у нас нет.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvoiceIssue {
    /// Многоразовый счёт или «любая сумма»: неясный исход оплаты — реальная двойная оплата.
    Reusable,
    AlreadyPaid,
    Expired,
    NoAmount,
    /// Не в USDT.
    UnsupportedAsset,
    /// В фиате: сумма в USDT плавает, такие пока не оплачиваем.
    Fiat,
    /// Карточка не прочиталась.
    Unreadable,
}

/// Почему счёт не взят в работу. Деньги клиента не тронуты.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvoiceReject {
    NotAnInvoice,
    /// Этот счёт уже в работе — заявка `order`.
    AlreadyInWork {
        order: String,
    },
    /// Этот счёт мы уже оплатили — заявка `order`.
    AlreadyPaid {
        order: String,
    },
    /// У клиента уже есть счёт, который ждёт чек.
    PendingInvoice {
        order: String,
    },
    DirectionClosed,
    ServicePaused,
    /// На кошельке платформы счёта нет столько; `max` — сколько можно оплатить сейчас.
    NoReserve {
        max: Decimal,
    },
    DailyLimit,
    /// Сколько прислать (`X`) меньше минимума направления.
    BelowMinimum {
        min: Decimal,
    },
    /// Сколько прислать (`X`) больше лимита клиента.
    AboveMaximum {
        max: Decimal,
    },
    Unsuitable(InvoiceIssue),
    /// Кошелёк не показал счёт (недоступен, FLOOD_WAIT): попробовать позже.
    WalletUnavailable,
}

impl InvoiceReject {
    pub const fn code(&self) -> &'static str {
        match self {
            InvoiceReject::NotAnInvoice => "not_an_invoice",
            InvoiceReject::AlreadyInWork { .. } => "invoice_in_work",
            InvoiceReject::AlreadyPaid { .. } => "invoice_already_paid_by_us",
            InvoiceReject::PendingInvoice { .. } => "invoice_pending",
            InvoiceReject::DirectionClosed => "direction_closed",
            InvoiceReject::ServicePaused => "service_paused",
            InvoiceReject::NoReserve { .. } => "no_reserve",
            InvoiceReject::DailyLimit => "daily_limit",
            InvoiceReject::BelowMinimum { .. } => "below_minimum",
            InvoiceReject::AboveMaximum { .. } => "above_maximum",
            InvoiceReject::Unsuitable(_) => "invoice_unsuitable",
            InvoiceReject::WalletUnavailable => "wallet_unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvoiceSubmitted {
    /// Котировка выдана: заявка ждёт чек клиента.
    Quoted(OrderRow),
    Rejected(InvoiceReject),
}

/// Направление по платформе счёта: счёт CryptoBot оплачиваем деньгами из xRocket и наоборот.
pub fn direction_for_invoice(platform: Platform) -> Direction {
    match platform {
        Platform::CryptoBot => Direction::PayCbInvoice,
        Platform::XRocket => Direction::PayXrInvoice,
    }
}

/// Сумма счёта, если его можно оплатить.
pub fn payable_amount(card: &InvoiceCard) -> Result<Decimal, InvoiceIssue> {
    match card.status {
        InvoiceStatus::Active => {}
        InvoiceStatus::Paid => return Err(InvoiceIssue::AlreadyPaid),
        InvoiceStatus::Expired => return Err(InvoiceIssue::Expired),
        InvoiceStatus::Unknown => return Err(InvoiceIssue::Unreadable),
    }
    if card.single_use == Some(false) {
        return Err(InvoiceIssue::Reusable);
    }
    if card.fiat.is_some() {
        return Err(InvoiceIssue::Fiat);
    }
    match card.amount {
        None => Err(InvoiceIssue::NoAmount),
        Some((_, asset)) if asset != Asset::Usdt => Err(InvoiceIssue::UnsupportedAsset),
        Some((amount, _)) if amount <= Decimal::ZERO => Err(InvoiceIssue::Unreadable),
        Some((amount, _)) => Ok(amount),
    }
}

/// Проверки до просмотра счёта и ещё раз в транзакции приёма.
async fn prechecks(
    conn: &mut PgConnection,
    user_id: i64,
    direction: Direction,
    link: &WalletLink,
) -> Result<Option<InvoiceReject>, EngineError> {
    if storage::settings::flag_on(&mut *conn, "maintenance").await?
        || storage::settings::flag_on(&mut *conn, "kill_switch_payouts").await?
    {
        return Ok(Some(InvoiceReject::ServicePaused));
    }
    if !storage::directions::get(&mut *conn, direction)
        .await?
        .accepting()
    {
        return Ok(Some(InvoiceReject::DirectionClosed));
    }
    if let Some(order) = storage::orders::by_invoice(&mut *conn, link.platform, &link.param).await?
    {
        return Ok(Some(if order.state == OrderState::Completed {
            InvoiceReject::AlreadyPaid {
                order: order.public_id,
            }
        } else {
            InvoiceReject::AlreadyInWork {
                order: order.public_id,
            }
        }));
    }
    if let Some(order) = storage::orders::awaiting_funds(&mut *conn, user_id, None).await? {
        return Ok(Some(InvoiceReject::PendingInvoice {
            order: order.public_id,
        }));
    }
    Ok(None)
}

impl Engine {
    /// Клиент `user_id` (уже есть в `users`) прислал ссылку на чужой счёт.
    pub async fn submit_invoice(
        &self,
        user_id: i64,
        link: &WalletLink,
    ) -> Result<InvoiceSubmitted, EngineError> {
        if link.kind != LinkKind::Invoice {
            return Ok(InvoiceSubmitted::Rejected(InvoiceReject::NotAnInvoice));
        }
        let direction = direction_for_invoice(link.platform);
        {
            let mut conn = self.pool.acquire().await?;
            if let Some(reason) = prechecks(&mut conn, user_id, direction, link).await? {
                return Ok(InvoiceSubmitted::Rejected(reason));
            }
        }

        // Открыть счёт, ничего не платя. Транзакция БД во время действия юзербота не держится.
        let wallet = self.wallet(link.platform)?.clone();
        let amount = match wallet.inspect_invoice(&link.param).await {
            Ok(info) => match payable_amount(&info.card) {
                Ok(amount) => amount,
                Err(issue) => {
                    tracing::info!(
                        ?issue,
                        platform = link.platform.db_name(),
                        "invoice not accepted"
                    );
                    return Ok(InvoiceSubmitted::Rejected(InvoiceReject::Unsuitable(issue)));
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, platform = link.platform.db_name(), "invoice could not be opened");
                return Ok(InvoiceSubmitted::Rejected(InvoiceReject::WalletUnavailable));
            }
        };

        let asset = Asset::Usdt;
        let mut tx = self.pool.begin().await?;
        if let Some(reason) = prechecks(&mut tx, user_id, direction, link).await? {
            return Ok(InvoiceSubmitted::Rejected(reason));
        }
        let dir = storage::directions::get(&mut tx, direction).await?;
        let target = direction.target();
        let (wallet_id, _) = storage::wallets::active(&mut tx, target)
            .await?
            .ok_or(EngineError::NoWalletAccount(target))?;
        storage::holds::lock_wallet(&mut tx, wallet_id).await?;
        let available = storage::holds::available(&mut tx, wallet_id, asset).await?;
        let fee_pct = fee_pct(&mut tx, &dir, wallet_id, asset, Decimal::ZERO).await?;
        // `X` клиент присылает чеком платформы входа: вверх до её шага.
        let step = Step::from_decimal(
            storage::directions::payout_step(&mut tx, direction.source(), asset).await?,
        )?;
        let quote = pricing::quote_invoice_payment(
            amount,
            FeePolicy {
                fee_pct,
                min_fee: dir.min_fee,
            },
            step,
        )?;
        let new_user = storage::orders::is_new_user(&mut tx, user_id).await?;
        let per_order = if new_user {
            dir.new_user_max_amount
        } else {
            dir.max_amount
        };
        let daily_left = match daily_limit(&mut tx, new_user).await? {
            Some(limit) => Some(limit - storage::orders::user_volume_24h(&mut tx, user_id).await?),
            None => None,
        };
        let room = available - self.cfg.reserve_safety;
        let reject = if quote.client_pays < dir.min_amount {
            Some(InvoiceReject::BelowMinimum {
                min: dir.min_amount,
            })
        } else if quote.client_pays > per_order {
            Some(InvoiceReject::AboveMaximum { max: per_order })
        } else if daily_left.is_some_and(|left| quote.client_pays > left) {
            Some(InvoiceReject::DailyLimit)
        } else if amount > room {
            Some(InvoiceReject::NoReserve {
                max: room.max(Decimal::ZERO),
            })
        } else {
            None
        };
        if let Some(reason) = reject {
            return Ok(InvoiceSubmitted::Rejected(reason));
        }

        let order = match storage::orders::insert(
            &mut tx,
            &NewOrder {
                user_id,
                direction,
                asset,
                intake_start_param: None,
                invoice_start_param: Some(&link.param),
            },
        )
        .await
        {
            Ok(order) => order,
            // Тот же счёт прислали одновременно в другой заявке.
            Err(StorageError::DuplicateInvoice) => {
                return Ok(InvoiceSubmitted::Rejected(InvoiceReject::AlreadyInWork {
                    order: String::new(),
                }));
            }
            Err(e) => return Err(e.into()),
        };
        let ttl = setting_decimal(&mut tx, "quote_ttl_seconds")
            .await?
            .and_then(|s| i64::try_from(s.trunc()).ok())
            .filter(|s| *s > 0)
            .unwrap_or(DEFAULT_QUOTE_TTL_SECS);
        let patch = OrderPatch {
            invoice_amount: Some(amount),
            quote_amount_in: Some(quote.client_pays),
            quote_expires_at: Some(Utc::now() + chrono::Duration::seconds(ttl)),
            fee_pct_applied: Some(fee_pct),
            fee_amount: Some(quote.fee),
            // У оплаты счёта «выплата» — сам счёт.
            payout_amount: Some(amount),
            ..OrderPatch::default()
        };
        let (order, _) = transit(
            &mut tx,
            &order,
            Event::LinkAccepted,
            &patch,
            None,
            EffectCtx {
                hold: Some((wallet_id, amount)),
                ..EffectCtx::default()
            },
        )
        .await?;
        tx.commit().await?;
        tracing::info!(order = order.id, %amount, to_pay = %quote.client_pays, "invoice quoted");
        self.notify(&order);
        Ok(InvoiceSubmitted::Quoted(order))
    }

    /// Чек клиента для заявки, которая ждёт деньги (`AWAITING_FUNDS`).
    pub(crate) async fn fund_invoice(
        &self,
        order_id: i64,
        link: &WalletLink,
    ) -> Result<Submitted, EngineError> {
        let rejected = |reason| {
            Ok(Submitted::Rejected {
                reason,
                order: None,
            })
        };
        let mut tx = self.pool.begin().await?;
        let order = storage::orders::get(&mut tx, order_id, true).await?;
        if order.state != OrderState::AwaitingFunds {
            return rejected(SubmitReject::QuoteExpired);
        }
        let d = storage::orders::details(&mut tx, order.id).await?;
        if d.quote_expires_at.is_some_and(|t| t <= Utc::now()) {
            return rejected(SubmitReject::QuoteExpired);
        }
        if storage::settings::flag_on(&mut tx, "maintenance").await?
            || storage::settings::flag_on(&mut tx, "kill_switch_payouts").await?
        {
            return rejected(SubmitReject::ServicePaused);
        }
        let patch = OrderPatch {
            intake_start_param: Some(&link.param),
            ..OrderPatch::default()
        };
        let order = match transit(
            &mut tx,
            &order,
            Event::ClientCheckSubmitted,
            &patch,
            None,
            EffectCtx::default(),
        )
        .await
        {
            Ok((order, _)) => order,
            // Этот чек уже в другой живой заявке.
            Err(EngineError::Storage(StorageError::DuplicateLiveCheck)) => {
                return rejected(SubmitReject::AlreadyInWork);
            }
            Err(e) => return Err(e),
        };
        tx.commit().await?;
        self.notify(&order);
        Ok(Submitted::Accepted(order))
    }

    /// Чек клиента зачислен по заявке на оплату счёта. Недоплата — снова ждём; хватает —
    /// оплата счёта (или человек). Всё в транзакции зачисления.
    pub(crate) async fn invoice_received(
        &self,
        conn: &mut PgConnection,
        order: &OrderRow,
        op_id: i64,
        received: &Money,
    ) -> Result<OrderRow, EngineError> {
        let d = storage::orders::details(&mut *conn, order.id).await?;
        let to_pay = d.quote_amount_in.ok_or_else(|| {
            EngineError::Invariant(format!("invoice order {}: no quote", order.id))
        })?;
        let same_asset =
            received.asset() == order.asset && d.intake_asset.is_none_or(|a| a == received.asset());
        // Чек в другом активе в сумму не складываем: его разберёт человек.
        let (total, fully_funded) = if same_asset {
            let total = d.intake_amount.unwrap_or_default() + received.amount();
            (total, total >= to_pay)
        } else {
            (d.intake_amount.unwrap_or(received.amount()), true)
        };
        let patch = OrderPatch {
            intake_amount: Some(total),
            intake_asset: Some(d.intake_asset.unwrap_or(received.asset())),
            ..OrderPatch::default()
        };
        let (order, _) = transit(
            &mut *conn,
            order,
            Event::CheckActivated { fully_funded },
            &patch,
            None,
            EffectCtx {
                cause_op: Some(op_id),
                intake_asset: Some(received.asset()),
                intake_delta: Some(received.amount()),
                ..EffectCtx::default()
            },
        )
        .await?;
        if !fully_funded {
            tracing::info!(order = order.id, %total, %to_pay, "invoice underpaid, waiting for more");
            return Ok(order);
        }
        let manual = if storage::settings::flag_on(&mut *conn, "kill_switch_payouts").await? {
            Some("kill switch: payouts are stopped".to_owned())
        } else if !same_asset {
            Some(format!(
                "check of {} {} for a {} invoice order",
                received.amount(),
                received.asset().code(),
                order.asset.code()
            ))
        } else {
            None
        };
        let (event, patch, reason) = match &manual {
            Some(reason) => (
                Event::Settle(Settlement::Manual),
                OrderPatch {
                    manual_reason: Some(reason),
                    ..OrderPatch::default()
                },
                Some(reason.as_str()),
            ),
            None => (
                Event::Settle(Settlement::PayInvoice),
                OrderPatch::default(),
                None,
            ),
        };
        let (order, _) = transit(
            &mut *conn,
            &order,
            event,
            &patch,
            reason,
            EffectCtx::default(),
        )
        .await?;
        Ok(order)
    }

    /// Переплата по оплаченному счёту, в транзакции `InvoicePaid` (до перехода): сколько
    /// вернуть чеком. `None` — переплаты нет или она остаётся в доходе.
    pub(crate) async fn excess_refund_amount(
        &self,
        conn: &mut PgConnection,
        order: &OrderRow,
    ) -> Result<Option<Decimal>, EngineError> {
        let d = storage::orders::details(&mut *conn, order.id).await?;
        let (Some(intake), Some(to_pay)) = (d.intake_amount, d.quote_amount_in) else {
            return Ok(None);
        };
        let excess = intake - to_pay;
        if excess <= Decimal::ZERO {
            return Ok(None);
        }
        let min_refund = setting_decimal(&mut *conn, "min_refund_amount")
            .await?
            .unwrap_or(Decimal::new(50, 2));
        if excess < min_refund {
            return Ok(None);
        }
        let asset = d.intake_asset.unwrap_or(order.asset);
        let received = Money::new(excess, asset)?;
        Ok(
            match refund(&mut *conn, order, &received, "excess").await? {
                Decision::Refund { amount, .. } => Some(amount),
                _ => None,
            },
        )
    }

    /// После `InvoicePaid`: переплата — чек на излишек (сумма уже в `refund_amount`) и
    /// остаток от округления в доход, либо вся переплата в доход.
    pub(crate) async fn settle_excess(
        &self,
        conn: &mut PgConnection,
        order: &OrderRow,
        cause_op: i64,
    ) -> Result<(), EngineError> {
        let d = storage::orders::details(&mut *conn, order.id).await?;
        let (Some(intake), Some(to_pay)) = (d.intake_amount, d.quote_amount_in) else {
            return Ok(());
        };
        let excess = intake - to_pay;
        if excess <= Decimal::ZERO {
            return Ok(());
        }
        let asset = d.intake_asset.unwrap_or(order.asset);
        let refunded = d.refund_amount.unwrap_or_default();
        let kept = excess - refunded;
        if kept > Decimal::ZERO {
            storage::ledger::post(
                &mut *conn,
                &ledger::excess_kept(asset, kept)?,
                PostingRefs {
                    order_id: Some(order.id),
                    operation_id: Some(cause_op),
                    memo: Some("invoice overpayment kept"),
                    ..Default::default()
                },
            )
            .await?;
        }
        if refunded > Decimal::ZERO {
            create_op(
                &mut *conn,
                order,
                &d,
                OpKind::CreateExcessCheck,
                MoneyRole::ExcessRefund,
            )
            .await?;
        }
        Ok(())
    }

    /// Котировки, истёкшие в ожидании чека: денег нет — `EXPIRED`, часть пришла — возврат.
    pub async fn expire_quotes(&self) -> Result<usize, EngineError> {
        let ids = {
            let mut conn = self.pool.acquire().await?;
            storage::orders::expired_quotes(&mut conn).await?
        };
        let mut done = 0;
        for id in ids {
            match self.close_awaiting(id, Close::Expired).await {
                Ok(true) => done += 1,
                Ok(false) => {}
                Err(e) => tracing::error!(order = id, error = %e, "quote expiry failed"),
            }
        }
        Ok(done)
    }

    /// Клиент отменил заявку на оплату счёта (кнопка «Отмена» или «Вернуть деньги»). Только в
    /// `AWAITING_FUNDS` и только свою. `false` — отменять нечего.
    pub async fn cancel_invoice(&self, user_id: i64, order_id: i64) -> Result<bool, EngineError> {
        {
            let mut conn = self.pool.acquire().await?;
            let order = storage::orders::get(&mut conn, order_id, false).await?;
            if order.user_id != user_id || order.direction.flow() != Flow::InvoicePayment {
                return Ok(false);
            }
        }
        self.close_awaiting(order_id, Close::Cancelled).await
    }

    async fn close_awaiting(&self, order_id: i64, how: Close) -> Result<bool, EngineError> {
        let mut tx = self.pool.begin().await?;
        let order = storage::orders::get(&mut tx, order_id, true).await?;
        if order.state != OrderState::AwaitingFunds {
            return Ok(false);
        }
        let d = storage::orders::details(&mut tx, order.id).await?;
        if how == Close::Expired && d.quote_expires_at.is_none_or(|t| t > Utc::now()) {
            return Ok(false);
        }
        let funds = match d.intake_amount {
            Some(amount) => Some(Money::new(amount, d.intake_asset.unwrap_or(order.asset))?),
            None => None,
        };
        let funds_received = funds.is_some();
        let event = match how {
            Close::Expired => Event::QuoteExpired { funds_received },
            Close::Cancelled => Event::ClientCancelled { funds_received },
        };
        let code = match how {
            Close::Expired => "quote_expired",
            Close::Cancelled => "client_cancelled",
        };
        // Пришедшее возвращаем целиком (за несостоявшуюся оплату комиссию не берём), вниз до
        // шага платформы входа. Пришедший чек не бывает меньше минимального; если всё же так —
        // пробуем вернуть всю сумму: не создастся — возврат уйдёт человеку (`RefundFailed`).
        let refund_amount = match &funds {
            Some(m) => Some(match refund(&mut tx, &order, m, code).await? {
                Decision::Refund { amount, .. } => amount,
                other => {
                    tracing::error!(
                        order = order.id,
                        ?other,
                        "partial payment is below the minimum check"
                    );
                    m.amount()
                }
            }),
            None => None,
        };
        let patch = OrderPatch {
            refund_amount,
            failure_code: Some(code),
            ..OrderPatch::default()
        };
        let (order, _) = transit(
            &mut tx,
            &order,
            event,
            &patch,
            Some(code),
            EffectCtx::default(),
        )
        .await?;
        tx.commit().await?;
        tracing::info!(order = order.id, state = ?order.state, code, "awaiting invoice order closed");
        self.notify(&order);
        Ok(true)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Close {
    Expired,
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn card(status: InvoiceStatus) -> InvoiceCard {
        InvoiceCard {
            status,
            amount: Some((dec!(10), Asset::Usdt)),
            fiat: None,
            single_use: Some(true),
            description: None,
        }
    }

    #[test]
    fn only_active_single_use_usdt_invoices_are_payable() {
        assert_eq!(payable_amount(&card(InvoiceStatus::Active)), Ok(dec!(10)));
        assert_eq!(
            payable_amount(&card(InvoiceStatus::Paid)),
            Err(InvoiceIssue::AlreadyPaid)
        );
        assert_eq!(
            payable_amount(&card(InvoiceStatus::Expired)),
            Err(InvoiceIssue::Expired)
        );
        assert_eq!(
            payable_amount(&card(InvoiceStatus::Unknown)),
            Err(InvoiceIssue::Unreadable)
        );
        let reusable = InvoiceCard {
            single_use: Some(false),
            ..card(InvoiceStatus::Active)
        };
        assert_eq!(payable_amount(&reusable), Err(InvoiceIssue::Reusable));
        let fiat = InvoiceCard {
            fiat: Some((dec!(900), "RUB".to_owned())),
            ..card(InvoiceStatus::Active)
        };
        assert_eq!(payable_amount(&fiat), Err(InvoiceIssue::Fiat));
        let ton = InvoiceCard {
            amount: Some((dec!(3), Asset::Ton)),
            ..card(InvoiceStatus::Active)
        };
        assert_eq!(payable_amount(&ton), Err(InvoiceIssue::UnsupportedAsset));
        let none = InvoiceCard {
            amount: None,
            ..card(InvoiceStatus::Active)
        };
        assert_eq!(payable_amount(&none), Err(InvoiceIssue::NoAmount));
    }
}
