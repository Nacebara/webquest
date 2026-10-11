//! Одно сообщение на заявку (DESIGN-v0.2 §6): отправляется при приёме чека и правится по
//! событиям движка. Id сообщения — в `orders.progress_*`, поэтому после рестарта правится то же
//! сообщение.
//!
//! Все пути сходятся в [`ensure_message`]: под блокировкой заявки прочитать её текущее
//! состояние из БД, нарисовать экран и править сообщение (или отправить, если его ещё нет).
//! Экран всегда строится по свежему состоянию, а не по событию: опоздавшее событие не вернёт
//! сообщение назад, а последнее событие всегда приводит к последнему состоянию.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use domain::fsm::OrderState;
use domain::{Decimal, Money};
use engine::OrderUpdate;
use storage::client::{OrderView, ProgressMessage};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use ui::{Breakdown, OrderCard, OrderRef, RefundReason, Screen, Ui, UiError};

use crate::api::Edited;
use crate::{Inner, TgBotError};

/// Сколько заявок пересинхронизировать за проход.
const RESYNC_BATCH: i64 = 500;

/// Можно ли отправить новое сообщение, если его ещё нет.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Create {
    /// Да: приём ссылки (в том числе отказ с записью заявки).
    Always,
    /// Да, кроме отказа при приёме: на него уже ответили экраном ошибки.
    UnlessRejected,
}

/// Данные из БД, которых нет в [`OrderView`], но нужны отдельным экранам.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Extra {
    /// Минимум направления (возврат «меньше минимума»).
    pub min_amount: Option<Decimal>,
    /// Максимум для этого клиента (возврат «больше лимита»).
    pub max_amount: Option<Decimal>,
    /// Сколько можно принять сейчас (отказ «нет резерва»).
    pub room: Option<Decimal>,
}

/// Настройки рендера, не зависящие от заявки.
pub(crate) struct RenderCfg<'a> {
    pub sla: chrono::Duration,
    pub bind_username: Option<&'a str>,
}

fn money(view: &OrderView, amount: Option<Decimal>) -> Option<Money> {
    amount.and_then(|a| Money::new(a, view.asset).ok())
}

fn breakdown(view: &OrderView) -> Option<Breakdown> {
    Some(Breakdown {
        amount_in: money(view, view.intake_amount)?,
        fee_pct: view.fee_pct_applied?.normalize(),
        fee: money(view, view.fee_amount)?,
        payout: money(view, view.payout_amount)?,
    })
}

/// Почему чек не принят кошельком (`failure_code` из движка).
fn intake_failure(code: Option<&str>, bind_username: Option<&str>) -> UiError {
    match code {
        Some("check_already_activated") => UiError::AlreadyActivated,
        Some("check_not_found") => UiError::NotFound,
        Some("check_not_for_us") => UiError::NotForUs {
            bind_username: bind_username.map(str::to_owned),
        },
        Some("check_needs_subscription") => UiError::NeedsSubscription,
        Some("check_captcha") => UiError::Captcha,
        Some("check_password") => UiError::PasswordProtected,
        Some("check_premium_only") => UiError::PremiumOnly,
        // Незнакомая причина: чек не открыт, деньги в нём — это и говорит общий экран.
        _ => UiError::Internal,
    }
}

/// Отказ при приёме (`engine::SubmitReject::code`). Чек не открывали.
fn reject_error(view: &OrderView, extra: &Extra) -> UiError {
    match view.failure_code.as_deref() {
        Some("direction_closed") => UiError::DirectionPaused {
            direction: view.direction,
            until: None,
        },
        Some("no_reserve") => UiError::NoReserve {
            max: money(view, extra.room).unwrap_or_else(|| Money::zero(view.asset)),
        },
        Some("already_in_work") => UiError::Duplicate {
            order: view.public_id.clone(),
        },
        Some("not_a_check") => UiError::NotCheckOrInvoice,
        _ => UiError::Internal,
    }
}

/// Причина возврата (`failure_code` при `Settle(Refund)`).
fn refund_reason(view: &OrderView, extra: &Extra) -> RefundReason {
    let code = view.failure_code.as_deref();
    match (
        code,
        money(view, extra.min_amount),
        money(view, extra.max_amount),
    ) {
        (Some("below_minimum" | "nothing_to_pay_out"), Some(min), _) => {
            RefundReason::BelowMinimum { min }
        }
        (Some("above_maximum" | "above_reserved_amount"), _, Some(max)) => {
            RefundReason::AboveMaximum { max }
        }
        _ => RefundReason::Operator,
    }
}

/// Карточка заявки (S11) по её виду.
pub(crate) fn card(view: &OrderView) -> OrderCard {
    let check_url = matches!(view.state, OrderState::Completed | OrderState::Refunded)
        .then(|| view.payout_check_url.clone())
        .flatten();
    OrderCard {
        order: OrderRef {
            id: view.public_id.clone(),
            direction: view.direction,
        },
        state: view.state,
        created_at: view.created_at,
        amount_in: money(view, view.intake_amount),
        fee: money(view, view.fee_amount),
        payout: money(view, view.payout_amount),
        invoice_amount: money(view, view.invoice_amount),
        refunded: money(view, view.refund_amount).filter(|_| view.state == OrderState::Refunded),
        check_url,
    }
}

/// Карточка заявки обычным HTML (её фолбэк) — для состояний без своего экрана в потоке.
fn card_plain(ui: &Ui, view: &OrderView) -> Screen {
    let screen = ui.order_card(&card(view));
    Screen {
        html: screen.html,
        rich_html: None,
        keyboard: screen.keyboard,
    }
}

/// Экран сообщения заявки для текущего состояния. Только обычный HTML и инлайн-кнопки:
/// сообщение правится `editMessageText`.
pub(crate) fn render(ui: &Ui, cfg: &RenderCfg<'_>, view: &OrderView, extra: &Extra) -> Screen {
    let order = OrderRef {
        id: view.public_id.clone(),
        direction: view.direction,
    };
    let finished =
        |view: &OrderView| -> DateTime<Utc> { view.finished_at.unwrap_or(view.state_since) };
    match view.state {
        OrderState::New | OrderState::IntakePending | OrderState::IntakeUnknown => {
            ui.order_checking(&order)
        }
        OrderState::Received | OrderState::PayoutPending => match money(view, view.payout_amount) {
            Some(payout) => ui.order_creating_payout(&order, &payout),
            None => ui.order_checking(&order),
        },
        OrderState::Completed => match (breakdown(view), view.payout_check_url.as_deref()) {
            (Some(b), Some(url)) => ui.order_done(&order, &b, url, finished(view)),
            _ => card_plain(ui, view),
        },
        OrderState::RefundPending => match money(view, view.refund_amount) {
            Some(refund) => ui.refund_started(&order, &refund, &refund_reason(view, extra)),
            None => card_plain(ui, view),
        },
        OrderState::Refunded => {
            match (
                money(view, view.refund_amount),
                view.payout_check_url.as_deref(),
            ) {
                (Some(refund), Some(url)) => ui.refund_done(&order, &refund, url),
                _ => card_plain(ui, view),
            }
        }
        OrderState::ManualReview => ui.manual_review(
            &order,
            money(view, view.intake_amount).as_ref(),
            view.state_since + cfg.sla,
        ),
        OrderState::IntakeFailed => ui.error(&intake_failure(
            view.failure_code.as_deref(),
            cfg.bind_username,
        )),
        OrderState::Rejected => ui.error(&reject_error(view, extra)),
        OrderState::AwaitingFunds
        | OrderState::InvoicePayPending
        | OrderState::Expired
        | OrderState::Cancelled
        | OrderState::ClosedManual => card_plain(ui, view),
    }
}

fn digest(screen: &Screen) -> u64 {
    let mut h = DefaultHasher::new();
    screen.html.hash(&mut h);
    format!("{:?}", screen.keyboard).hash(&mut h);
    h.finish()
}

impl Inner {
    /// Данные для экранов возврата и отказа «нет резерва».
    async fn extra(&self, view: &OrderView) -> Result<Extra, TgBotError> {
        let mut extra = Extra::default();
        match view.state {
            OrderState::RefundPending => {
                let mut conn = self.pool.acquire().await?;
                let dir = storage::directions::get(&mut conn, view.direction).await?;
                let new_user = storage::orders::is_new_user(&mut conn, view.user_id).await?;
                extra.min_amount = Some(dir.min_amount);
                extra.max_amount = Some(if new_user {
                    dir.new_user_max_amount
                } else {
                    dir.max_amount
                });
                if view.failure_code.as_deref() == Some("above_reserved_amount") {
                    let terms = self.terms().await?;
                    extra.max_amount = terms
                        .terms
                        .get(view.direction)
                        .map(|t| t.max)
                        .or(extra.max_amount);
                }
            }
            OrderState::Rejected => {
                extra.room = self.terms().await?.room.get(&view.direction).copied();
            }
            _ => {}
        }
        Ok(extra)
    }

    async fn store_progress(
        &self,
        order_id: i64,
        message: ProgressMessage,
    ) -> Result<(), TgBotError> {
        let mut conn = self.pool.acquire().await?;
        storage::client::set_progress_message(&mut conn, order_id, message).await?;
        Ok(())
    }

    fn remember_rendered(&self, view: &OrderView, digest: u64) {
        let terminal = view.state.is_terminal();
        self.memory(|m| {
            if terminal {
                // Терминальное сообщение больше не меняется: помнить незачем.
                m.rendered.remove(&view.id);
            } else {
                m.rendered.insert(view.id, digest);
            }
        });
    }

    async fn send_progress(
        &self,
        view: &OrderView,
        chat: i64,
        screen: &Screen,
        digest: u64,
    ) -> Result<(), TgBotError> {
        match self.api.send_html(chat, screen).await {
            Ok(message) => {
                self.store_progress(
                    view.id,
                    ProgressMessage {
                        chat_id: message.chat.id,
                        message_id: message.message_id,
                    },
                )
                .await?;
                self.remember_rendered(view, digest);
                tracing::info!(
                    order = view.id,
                    state = view.state.as_db_str(),
                    "order message sent"
                );
                Ok(())
            }
            Err(e) if e.is_forbidden() => {
                tracing::warn!(order = view.id, error = %e, "client blocked the bot, order message not delivered");
                self.memory(|m| m.unreachable.insert(view.id));
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// Привести сообщение заявки к её текущему состоянию.
pub(crate) async fn ensure_message(
    inner: &Arc<Inner>,
    order_id: i64,
    create: Create,
) -> Result<(), TgBotError> {
    let _guard = inner.locks.lock(order_id).await;
    let view = {
        let mut conn = inner.pool.acquire().await?;
        storage::client::order_view(&mut conn, order_id).await?
    };
    let Some(view) = view else {
        return Ok(());
    };
    if inner.memory(|m| m.unreachable.contains(&order_id)) {
        return Ok(());
    }
    let extra = inner.extra(&view).await?;
    let cfg = RenderCfg {
        sla: chrono::Duration::from_std(inner.cfg.manual_review_sla).unwrap_or_default(),
        bind_username: inner.cfg.bind_username.as_deref(),
    };
    let screen = render(&inner.ui, &cfg, &view, &extra);
    let digest = digest(&screen);
    match view.progress {
        Some(p) => {
            if inner.memory(|m| m.rendered.get(&order_id) == Some(&digest)) {
                return Ok(());
            }
            match inner.api.edit_html(p.chat_id, p.message_id, &screen).await {
                Ok(Edited::Done | Edited::NotModified) => {
                    inner.remember_rendered(&view, digest);
                    tracing::debug!(
                        order = order_id,
                        state = view.state.as_db_str(),
                        "order message updated"
                    );
                    Ok(())
                }
                // Клиент удалил сообщение: итог всё равно должен до него дойти.
                Ok(Edited::Gone) => inner.send_progress(&view, p.chat_id, &screen, digest).await,
                Err(e) if e.is_forbidden() => {
                    inner.memory(|m| m.unreachable.insert(order_id));
                    Ok(())
                }
                Err(e) => Err(e.into()),
            }
        }
        None => {
            let allowed = match create {
                Create::Always => true,
                Create::UnlessRejected => view.state != OrderState::Rejected,
            };
            if !allowed {
                return Ok(());
            }
            // Бот работает только в личке: чат клиента = его id.
            inner
                .send_progress(&view, view.user_id, &screen, digest)
                .await
        }
    }
}

/// Сверить сообщения заявок с состоянием: после старта — все свежие, в фоне — только те,
/// сообщение которых так и не ушло.
async fn resync(inner: &Arc<Inner>, missing_only: bool) {
    let since =
        Utc::now() - chrono::Duration::from_std(inner.cfg.resync_window).unwrap_or_default();
    let ids = {
        let conn = inner.pool.acquire().await;
        match conn {
            Ok(mut conn) => {
                storage::client::orders_to_resync(&mut conn, since, missing_only, RESYNC_BATCH)
                    .await
            }
            Err(e) => Err(e.into()),
        }
    };
    let ids = match ids {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(error = %e, "order messages resync skipped");
            return;
        }
    };
    if !ids.is_empty() {
        tracing::info!(count = ids.len(), missing_only, "resyncing order messages");
    }
    for id in ids {
        if let Err(e) = ensure_message(inner, id, Create::UnlessRejected).await {
            tracing::warn!(order = id, error = %e, "order message resync failed");
        }
    }
}

/// События движка → правка сообщений. Плюс пересинхронизация при старте, при потере событий
/// (переполнение канала) и периодическая досылка неотправленных.
pub(crate) async fn listen(
    inner: Arc<Inner>,
    mut updates: broadcast::Receiver<OrderUpdate>,
    cancel: CancellationToken,
) {
    resync(&inner, false).await;
    let mut sweep = tokio::time::interval(inner.cfg.sweep_interval);
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    sweep.tick().await;
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = sweep.tick() => {
                resync(&inner, true).await;
                inner.limiter.purge(std::time::Instant::now());
            }
            update = updates.recv() => match update {
                Ok(update) => {
                    let inner = inner.clone();
                    tasks.spawn(async move {
                        if let Err(e) =
                            ensure_message(&inner, update.order_id, Create::UnlessRejected).await
                        {
                            tracing::warn!(order = update.order_id, error = %e, "order message not updated");
                        }
                    });
                }
                Err(RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "order updates lost, resyncing all order messages");
                    resync(&inner, false).await;
                }
                Err(RecvError::Closed) => break,
            },
            Some(done) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(e) = done {
                    tracing::error!(error = %e, "order message task crashed");
                }
            }
        }
    }
    let grace = inner.cfg.shutdown_grace;
    let drained =
        tokio::time::timeout(grace, async { while tasks.join_next().await.is_some() {} }).await;
    if drained.is_err() {
        tracing::warn!("order message updates still running at shutdown, aborted");
        tasks.abort_all();
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use domain::{Asset, Direction};
    use rust_decimal_macros::dec;
    use ui::{EmojiSet, UiConfig};

    use super::*;

    fn ui() -> Ui {
        Ui::builtin(EmojiSet::plain(), UiConfig::default()).expect("встроенные тексты")
    }

    fn view(state: OrderState) -> OrderView {
        let at = Utc
            .with_ymd_and_hms(2026, 10, 7, 12, 0, 0)
            .single()
            .unwrap_or_default();
        OrderView {
            id: 1,
            public_id: "A1B2C3D4E5".into(),
            user_id: 7,
            direction: Direction::CbToXrCheck,
            asset: Asset::Usdt,
            state,
            intake_amount: Some(dec!(100)),
            fee_pct_applied: Some(dec!(2.500000)),
            fee_amount: Some(dec!(2.5)),
            payout_amount: Some(dec!(97.5)),
            refund_amount: None,
            invoice_amount: None,
            payout_check_url: None,
            failure_code: None,
            created_at: at,
            finished_at: None,
            state_since: at,
            progress: None,
        }
    }

    fn cfg() -> RenderCfg<'static> {
        RenderCfg {
            sla: chrono::Duration::minutes(30),
            bind_username: None,
        }
    }

    fn buttons(screen: &Screen) -> Vec<botapi::InlineButtonAction> {
        screen
            .keyboard
            .inline()
            .map(|m| m.buttons().map(|b| b.action.clone()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn every_state_renders_plain_html_with_inline_or_no_keyboard() {
        let ui = ui();
        for state in OrderState::ALL {
            let screen = render(&ui, &cfg(), &view(state), &Extra::default());
            assert!(!screen.html.is_empty(), "{state:?}");
            assert!(
                screen.rich_html.is_none(),
                "{state:?}: денежный поток — обычный HTML"
            );
            assert!(
                matches!(
                    screen.keyboard,
                    ui::Keyboard::Inline(_) | ui::Keyboard::None
                ),
                "{state:?}: править можно только с инлайн-клавиатурой"
            );
        }
    }

    #[test]
    fn completed_has_claim_url_and_copy_link() {
        let ui = ui();
        let url = "https://t.me/xrocket?start=t_val6vkkTbAbxd06";
        let mut v = view(OrderState::Completed);
        v.payout_check_url = Some(url.into());
        let screen = render(&ui, &cfg(), &v, &Extra::default());
        let actions = buttons(&screen);
        assert!(actions.contains(&botapi::InlineButtonAction::Url(url.into())));
        assert!(actions.contains(&botapi::InlineButtonAction::CopyText(url.into())));
        assert!(screen.html.contains("97.50 USDT"), "{}", screen.html);
    }

    #[test]
    fn failures_map_to_their_error_screens() {
        let ui = ui();
        let cases = [
            (
                OrderState::IntakeFailed,
                "check_already_activated",
                UiError::AlreadyActivated,
            ),
            (
                OrderState::IntakeFailed,
                "check_password",
                UiError::PasswordProtected,
            ),
            (OrderState::IntakeFailed, "something_new", UiError::Internal),
            (
                OrderState::Rejected,
                "direction_closed",
                UiError::DirectionPaused {
                    direction: Direction::CbToXrCheck,
                    until: None,
                },
            ),
            (
                OrderState::Rejected,
                "not_a_check",
                UiError::NotCheckOrInvoice,
            ),
        ];
        for (state, code, expected) in cases {
            let mut v = view(state);
            v.failure_code = Some(code.into());
            let screen = render(&ui, &cfg(), &v, &Extra::default());
            assert_eq!(screen, ui.error(&expected), "{code}");
        }
        let mut v = view(OrderState::Rejected);
        v.failure_code = Some("no_reserve".into());
        let extra = Extra {
            room: Some(dec!(42.5)),
            ..Extra::default()
        };
        let screen = render(&ui, &cfg(), &v, &extra);
        assert!(screen.html.contains("42.50 USDT"), "{}", screen.html);
    }

    #[test]
    fn refund_reason_uses_limits() {
        let mut v = view(OrderState::RefundPending);
        v.refund_amount = Some(dec!(1.5));
        v.failure_code = Some("below_minimum".into());
        let extra = Extra {
            min_amount: Some(dec!(2)),
            max_amount: Some(dec!(100)),
            room: None,
        };
        assert_eq!(
            refund_reason(&v, &extra),
            RefundReason::BelowMinimum {
                min: Money::new(dec!(2), Asset::Usdt).unwrap_or_else(|_| Money::zero(Asset::Usdt))
            }
        );
        v.failure_code = Some("above_maximum".into());
        assert!(matches!(
            refund_reason(&v, &extra),
            RefundReason::AboveMaximum { .. }
        ));
        v.failure_code = None;
        assert_eq!(refund_reason(&v, &extra), RefundReason::Operator);
        let screen = render(&ui(), &cfg(), &v, &extra);
        assert!(screen.html.contains("1.50 USDT"), "{}", screen.html);
    }

    #[test]
    fn manual_review_promises_sla_and_says_where_money_is() {
        let ui = ui();
        let known = render(
            &ui,
            &cfg(),
            &view(OrderState::ManualReview),
            &Extra::default(),
        );
        assert!(known.html.contains("100.00 USDT"), "{}", known.html);
        assert!(
            known.html.contains("12:30") || known.html.contains("15:30"),
            "{}",
            known.html
        );
        let mut v = view(OrderState::ManualReview);
        v.intake_amount = None;
        let unknown = render(&ui, &cfg(), &v, &Extra::default());
        assert_ne!(known.html, unknown.html);
    }
}
