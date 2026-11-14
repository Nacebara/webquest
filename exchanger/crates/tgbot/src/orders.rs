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
use domain::fsm::{Flow, OrderState};
use domain::{Decimal, Money};
use engine::OrderUpdate;
use storage::client::{OrderView, ProgressMessage};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use ui::{Breakdown, Excess, InvoiceQuote, OrderCard, OrderRef, RefundReason, Screen, Ui, UiError};

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
    /// Меньше этой переплаты по счёту не возвращаем (`settings.min_refund_amount`).
    pub min_refund: Option<Decimal>,
}

/// Настройки рендера, не зависящие от заявки.
pub(crate) struct RenderCfg<'a> {
    pub sla: chrono::Duration,
    pub bind_username: Option<&'a str>,
    /// Клиент развернул «Детали» итога (макет B1).
    pub details: bool,
    /// Сейчас: шаг дольше [`SLOW_AFTER`] получает строку «Дольше обычного».
    pub now: DateTime<Utc>,
}

/// Через сколько секунд от начала заявки шаг считается долгим (макет B1: 20 секунд).
pub(crate) const SLOW_AFTER: std::time::Duration = std::time::Duration::from_secs(20);

/// Итог позже этого после отправки сообщения заявки — новым сообщением (правило макета:
/// «если прошло больше минуты — итог новым сообщением, чтобы пришло уведомление»; шаг 6.1).
pub(crate) const LATE_AFTER: std::time::Duration = std::time::Duration::from_secs(60);

/// Итог заявки, о котором клиенту должно прийти уведомление, — и пришёл он поздно: позже
/// [`LATE_AFTER`] после отправки её сообщения. После того как итог ушёл новым сообщением, время
/// отправки — уже после итога, и второй раз он не уйдёт. Заявки без времени отправки (старше
/// миграции 0006) — как раньше.
fn late_result(view: &OrderView) -> bool {
    let result = matches!(
        view.state,
        OrderState::Completed
            | OrderState::Refunded
            | OrderState::ClosedManual
            | OrderState::IntakeFailed
            | OrderState::Expired
    );
    let late = chrono::Duration::from_std(LATE_AFTER).unwrap_or_default();
    result
        && view
            .progress_sent_at
            .is_some_and(|sent| view.state_since - sent > late)
}

/// В прежнем шаге деньги клиента были у сервиса — строка «Деньги у меня».
fn money_was_with_us(state: OrderState) -> bool {
    matches!(
        state,
        OrderState::Received
            | OrderState::PayoutPending
            | OrderState::InvoicePayPending
            | OrderState::RefundPending
            | OrderState::ManualReview
    )
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
pub(crate) fn intake_failure(code: Option<&str>, bind_username: Option<&str>) -> UiError {
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
        (Some("quote_expired"), _, _) => RefundReason::Underpaid,
        (Some("client_cancelled"), _, _) => RefundReason::ClientRequest,
        (Some("invoice_unpayable"), _, _) => RefundReason::InvoiceExpired,
        (Some("rate_moved"), _, _) => RefundReason::RateMoved,
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
        fee_pct: view.fee_pct_applied.map(|p| p.normalize()),
        finished_at: view.finished_at,
    }
}

/// Карточка заявки обычным HTML (её фолбэк) — для состояний без своего экрана в потоке.
fn card_plain(ui: &Ui, view: &OrderView) -> Screen {
    let screen = ui.order_card(&card(view), Utc::now());
    Screen {
        html: screen.html,
        rich_html: None,
        keyboard: screen.keyboard,
    }
}

/// Котировка заявки на оплату счёта.
pub(crate) fn quote(view: &OrderView) -> Option<InvoiceQuote> {
    Some(InvoiceQuote {
        order: OrderRef {
            id: view.public_id.clone(),
            direction: view.direction,
        },
        invoice_amount: money(view, view.invoice_amount)?,
        fiat: match (&view.invoice_fiat, view.invoice_fiat_amount) {
            (Some(currency), Some(amount)) => Some(ui::FiatAmount {
                amount,
                currency: currency.clone(),
                buffer_pct: view.fiat_buffer_pct,
            }),
            _ => None,
        },
        description: None,
        fee_pct: view.fee_pct_applied?.normalize(),
        fee: money(view, view.fee_amount)?,
        to_pay: money(view, view.quote_amount_in)?,
        expires_at: view.quote_expires_at?,
    })
}

/// Экраны, свои у оплаты счёта (S5–S7). `None` — общий экран заявки.
fn render_invoice(ui: &Ui, cfg: &RenderCfg<'_>, view: &OrderView, extra: &Extra) -> Option<Screen> {
    let q = quote(view)?;
    let got = money(view, view.intake_amount);
    match view.state {
        OrderState::AwaitingFunds if view.last_check_rejected => Some(ui.invoice_check_rejected(
            &q,
            &intake_failure(view.failure_code.as_deref(), cfg.bind_username),
            cfg.now,
        )),
        OrderState::AwaitingFunds => Some(match got {
            Some(got) => {
                let rest = Money::new(q.to_pay.amount() - got.amount(), got.asset()).ok()?;
                ui.invoice_underpaid(&q, &got, &rest)
            }
            None => ui.invoice_quote(&q, cfg.now),
        }),
        OrderState::Received | OrderState::InvoicePayPending => Some(ui.invoice_paying(&q)),
        OrderState::Completed => {
            let excess = got
                .map(|g| g.amount() - q.to_pay.amount())
                .filter(|e| *e > Decimal::ZERO);
            let excess = match (excess, money(view, view.refund_amount)) {
                (None, _) => None,
                (Some(_), Some(refund)) => Some(Excess::Refund(refund)),
                (Some(amount), None) => Some(Excess::Kept {
                    amount: money(view, Some(amount))?,
                    min_refund: money(view, extra.min_refund)?,
                }),
            };
            Some(ui.invoice_paid(
                &q,
                excess.as_ref(),
                view.payout_check_url.as_deref(),
                view.finished_at.unwrap_or(view.state_since),
            ))
        }
        OrderState::Expired => Some(ui.error(&UiError::QuoteExpired)),
        OrderState::Cancelled => Some(ui.cancelled(Some(&view.public_id))),
        _ => None,
    }
}

/// Экран сообщения заявки для текущего состояния. Только обычный HTML и инлайн-кнопки:
/// сообщение правится `editMessageText`.
pub(crate) fn render(ui: &Ui, cfg: &RenderCfg<'_>, view: &OrderView, extra: &Extra) -> Screen {
    if view.direction.flow() == Flow::InvoicePayment
        && let Some(screen) = render_invoice(ui, cfg, view, extra)
    {
        return screen;
    }
    let order = OrderRef {
        id: view.public_id.clone(),
        direction: view.direction,
    };
    let slow =
        cfg.now - view.created_at >= chrono::Duration::from_std(SLOW_AFTER).unwrap_or_default();
    let intake = money(view, view.intake_amount);
    match view.state {
        OrderState::New | OrderState::IntakePending | OrderState::IntakeUnknown => {
            ui.order_checking(&order, intake.as_ref(), slow)
        }
        OrderState::Received | OrderState::PayoutPending => match money(view, view.payout_amount) {
            Some(payout) => ui.order_sending(&order, &payout, slow),
            None => ui.order_checking(&order, intake.as_ref(), slow),
        },
        OrderState::Completed => match (breakdown(view), view.payout_check_url.as_deref()) {
            (Some(b), Some(url)) => {
                let finished = view.finished_at.unwrap_or(view.state_since);
                let took = (finished - view.created_at).num_seconds();
                ui.order_done(&order, &b, url, took, cfg.details)
            }
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
                (Some(refund), Some(url)) => {
                    ui.refund_done(&order, &refund, url, Some(&refund_reason(view, extra)))
                }
                _ => card_plain(ui, view),
            }
        }
        OrderState::ManualReview => ui.manual_review(
            &order,
            money(view, view.intake_amount).as_ref(),
            cfg.sla.num_minutes(),
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
    screen.rich_html.hash(&mut h);
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
                let dir = storage::user_terms::effective(&mut conn, view.direction, view.user_id)
                    .await?
                    .dir;
                let new_user = storage::orders::is_new_user(&mut conn, view.user_id).await?;
                extra.min_amount = Some(dir.min_amount);
                extra.max_amount = Some(if new_user {
                    dir.new_user_max_amount
                } else {
                    dir.max_amount
                });
                if view.failure_code.as_deref() == Some("above_reserved_amount") {
                    let terms = self.terms_for(view.user_id).await?;
                    extra.max_amount = terms
                        .terms
                        .get(view.direction)
                        .map(|t| t.max)
                        .or(extra.max_amount);
                }
            }
            OrderState::Rejected => {
                extra.room = self
                    .terms_for(view.user_id)
                    .await?
                    .room
                    .get(&view.direction)
                    .copied();
            }
            OrderState::Completed if view.direction.flow() == Flow::InvoicePayment => {
                let mut conn = self.pool.acquire().await?;
                extra.min_refund = storage::settings::get(&mut conn, "min_refund_amount")
                    .await?
                    .and_then(|v| v.to_string().trim_matches('"').parse().ok());
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

    /// Сообщение денежного потока: «Квитанция» — rich (если клиент не просил простой вид),
    /// Telegram отказал по существу — тот же экран HTML; остальные шаги — HTML.
    pub(crate) async fn send_flow(
        &self,
        chat: i64,
        user: i64,
        screen: &Screen,
    ) -> Result<botapi::Message, botapi::Error> {
        match screen.rich_html.as_deref() {
            Some(rich) if !self.prefers_plain(user) => self
                .api
                .send_rich(chat, rich, &screen.keyboard, screen)
                .await
                .map(|(message, _)| message),
            _ => self.api.send_html(chat, screen).await,
        }
    }

    /// Править сообщение денежного потока тем же правилом, что [`Inner::send_flow`]: шаг
    /// после квитанции (обычный HTML) правит rich-сообщение на месте.
    pub(crate) async fn edit_flow(
        &self,
        chat: i64,
        message_id: i32,
        user: i64,
        screen: &Screen,
    ) -> Result<Edited, botapi::Error> {
        if let Some(rich) = screen.rich_html.as_deref()
            && !self.prefers_plain(user)
        {
            match self
                .api
                .edit_rich(chat, message_id, rich, &screen.keyboard)
                .await
            {
                Err(e) if e.is_rejection() => {
                    tracing::info!(error = %e, "rich edit rejected, editing with the HTML fallback");
                }
                other => return other,
            }
        }
        self.api.edit_html(chat, message_id, screen).await
    }

    async fn send_progress(
        &self,
        view: &OrderView,
        chat: i64,
        screen: &Screen,
        digest: u64,
    ) -> Result<(), TgBotError> {
        match self.send_flow(chat, view.user_id, screen).await {
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
        details: inner.memory(|m| m.details_open.contains(&order_id)),
        now: Utc::now(),
    };
    let screen = render(&inner.ui(), &cfg, &view, &extra);
    let digest = digest(&screen);
    match view.progress {
        Some(p) if late_result(&view) => {
            // Сначала итог новым сообщением (со звуком), потом — «Итог — ниже» в прежнем.
            inner
                .send_progress(&view, p.chat_id, &screen, digest)
                .await?;
            if inner.memory(|m| m.unreachable.contains(&order_id)) {
                return Ok(());
            }
            let ui = inner.ui();
            let step = view.previous_state.map(|prev| {
                let was = OrderView {
                    state: prev,
                    ..view.clone()
                };
                render(&ui, &cfg, &was, &extra)
            });
            let with_me = view
                .previous_state
                .filter(|s| money_was_with_us(*s))
                .and_then(|_| money(&view, view.intake_amount));
            let below = ui.result_below(step.as_ref().unwrap_or(&screen), with_me.as_ref());
            if let Err(e) = inner
                .edit_flow(p.chat_id, p.message_id, view.user_id, &below)
                .await
            {
                tracing::debug!(order = order_id, error = %e, "old order message not marked «result below»");
            }
            tracing::info!(
                order = order_id,
                state = view.state.as_db_str(),
                "late result sent as a new message"
            );
            Ok(())
        }
        Some(p) => {
            if inner.memory(|m| m.rendered.get(&order_id) == Some(&digest)) {
                return Ok(());
            }
            match inner
                .edit_flow(p.chat_id, p.message_id, view.user_id, &screen)
                .await
            {
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
                .await?;
            // Шаг затянется — то же сообщение получит «Дольше обычного» (макет B1).
            if !view.state.is_terminal() {
                let inner = inner.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(SLOW_AFTER + std::time::Duration::from_secs(1)).await;
                    if let Err(e) = refresh(inner, order_id).await {
                        tracing::debug!(order = order_id, error = %e, "slow-step refresh failed");
                    }
                });
            }
            Ok(())
        }
    }
}

/// Перерисовать сообщение заявки из фоновой задачи. Явный `Send`-тип разрывает рекурсию
/// типов: задачу запускает сам [`ensure_message`].
fn refresh(
    inner: Arc<Inner>,
    order_id: i64,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), TgBotError>> + Send>> {
    Box::pin(async move { ensure_message(&inner, order_id, Create::UnlessRejected).await })
}

/// «Пришлю чек»: сообщение заявки на оплату счёта — экраном ожидания чека (S6).
pub(crate) async fn show_awaiting(inner: &Arc<Inner>, view: &OrderView) -> Result<(), TgBotError> {
    if view.state != OrderState::AwaitingFunds || view.intake_amount.is_some() {
        return ensure_message(inner, view.id, Create::UnlessRejected).await;
    }
    let (Some(q), Some(p)) = (quote(view), view.progress) else {
        return Ok(());
    };
    let _guard = inner.locks.lock(view.id).await;
    let screen = inner.ui().invoice_awaiting(&q, Utc::now());
    match inner
        .edit_flow(p.chat_id, p.message_id, view.user_id, &screen)
        .await
    {
        Ok(_) => {
            inner.remember_rendered(view, digest(&screen));
            Ok(())
        }
        Err(e) if e.is_forbidden() => Ok(()),
        Err(e) => Err(e.into()),
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
            quote_amount_in: None,
            quote_expires_at: None,
            invoice_fiat: None,
            invoice_fiat_amount: None,
            fiat_buffer_pct: dec!(1),
            last_check_rejected: false,
            payout_check_url: None,
            failure_code: None,
            created_at: at,
            finished_at: None,
            state_since: at,
            progress: None,
            progress_sent_at: None,
            previous_state: None,
        }
    }

    /// Заявка на оплату счёта CryptoBot 10 USDT: прислать 10,30 чеком xRocket.
    fn invoice_view(state: OrderState) -> OrderView {
        OrderView {
            direction: Direction::PayCbInvoice,
            intake_amount: None,
            fee_pct_applied: Some(dec!(3)),
            fee_amount: Some(dec!(0.30)),
            payout_amount: Some(dec!(10)),
            invoice_amount: Some(dec!(10)),
            quote_amount_in: Some(dec!(10.30)),
            quote_expires_at: Some(view(state).created_at + chrono::Duration::minutes(15)),
            ..view(state)
        }
    }

    /// Текст экрана без разметки, с обычными пробелами: «<code>10.30</code>&nbsp;USDT» → «10.30 USDT».
    fn html(screen: &Screen) -> String {
        let mut out = String::new();
        let mut tag = false;
        for c in screen.html.chars() {
            match c {
                '<' => tag = true,
                '>' => tag = false,
                '\u{a0}' if !tag => out.push(' '),
                c if !tag => out.push(c),
                _ => {}
            }
        }
        out
    }

    #[test]
    fn invoice_states_have_their_own_screens() {
        let ui = ui();
        let extra = Extra {
            min_refund: Some(dec!(0.5)),
            ..Extra::default()
        };
        let quote = render(
            &ui,
            &cfg(),
            &invoice_view(OrderState::AwaitingFunds),
            &extra,
        );
        // «Квитанция» (выбор владельца 8 ноября 2026): итог столбиком, под сообщением —
        // одна синяя кнопка с командой кошелька и суммой, ниже «Отмена»; rich и HTML запасом.
        assert!(html(&quote).contains("10.30"), "{}", quote.html);
        assert!(quote.rich_html.is_some(), "квитанция — rich");
        assert!(
            buttons(&quote).contains(&botapi::InlineButtonAction::CopyText(
                "@xrocket 10.30".into()
            )),
            "{:?}",
            buttons(&quote)
        );
        assert!(
            buttons(&quote).iter().any(|b| matches!(
                b,
                botapi::InlineButtonAction::CallbackData(d) if d.starts_with("cancel:")
            )),
            "{:?}",
            buttons(&quote)
        );

        let underpaid = OrderView {
            intake_amount: Some(dec!(7)),
            ..invoice_view(OrderState::AwaitingFunds)
        };
        let screen = render(&ui, &cfg(), &underpaid, &extra);
        assert!(html(&screen).contains("3.30 USDT"), "{}", screen.html);

        let rejected = OrderView {
            last_check_rejected: true,
            failure_code: Some("check_already_activated".into()),
            ..invoice_view(OrderState::AwaitingFunds)
        };
        let screen = render(&ui, &cfg(), &rejected, &extra);
        assert!(
            html(&screen).contains("Этот чек уже открыт"),
            "{}",
            screen.html
        );
        assert!(html(&screen).contains("Жду чек"), "{}", screen.html);

        let paying = render(
            &ui,
            &cfg(),
            &invoice_view(OrderState::InvoicePayPending),
            &extra,
        );
        assert!(html(&paying).contains("Оплачиваю счёт"), "{}", paying.html);

        let paid_with_excess = OrderView {
            intake_amount: Some(dec!(15)),
            refund_amount: Some(dec!(4.70)),
            payout_check_url: Some("https://t.me/xrocket?start=t_abcdefghijklmno".into()),
            ..invoice_view(OrderState::Completed)
        };
        let screen = render(&ui, &cfg(), &paid_with_excess, &extra);
        assert!(html(&screen).contains("Счёт оплачен"), "{}", screen.html);
        assert!(html(&screen).contains("4.70 USDT"), "{}", screen.html);
        assert!(buttons(&screen).iter().any(|b| matches!(
            b,
            botapi::InlineButtonAction::Url(u) if u.contains("t_abcdefghijklmno")
        )));

        let kept = OrderView {
            intake_amount: Some(dec!(10.50)),
            ..invoice_view(OrderState::Completed)
        };
        let screen = render(&ui, &cfg(), &kept, &extra);
        assert!(html(&screen).contains("0.20 USDT"), "{}", screen.html);

        let expired = render(&ui, &cfg(), &invoice_view(OrderState::Expired), &extra);
        assert!(html(&expired).contains("Время вышло"), "{}", expired.html);
    }

    fn cfg() -> RenderCfg<'static> {
        RenderCfg {
            sla: chrono::Duration::minutes(30),
            bind_username: None,
            details: false,
            now: Utc::now(),
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
    fn completed_is_b1_claim_details_again() {
        let ui = ui();
        let url = "https://t.me/xrocket?start=t_val6vkkTbAbxd06";
        let mut v = view(OrderState::Completed);
        v.payout_check_url = Some(url.into());
        let screen = render(&ui, &cfg(), &v, &Extra::default());
        let actions = buttons(&screen);
        assert_eq!(
            actions.first(),
            Some(&botapi::InlineButtonAction::Url(url.into()))
        );
        let det = ui::Callback::Details(v.public_id.clone()).encode();
        assert!(actions.contains(&botapi::InlineButtonAction::CallbackData(det)));
        assert!(
            screen
                .html
                .contains("97.50\u{a0}USDT</b> ждут вас в xRocket"),
            "{}",
            screen.html
        );
        // «Детали» — то же сообщение с разбивкой и номером заявки, кнопка «Скрыть».
        let open = RenderCfg {
            details: true,
            ..cfg()
        };
        let screen = render(&ui, &open, &v, &Extra::default());
        assert!(screen.html.contains("Получено 100.00"), "{}", screen.html);
        assert!(screen.html.contains(&v.public_id), "{}", screen.html);
        let hide = ui::Callback::HideDetails(v.public_id.clone()).encode();
        assert!(buttons(&screen).contains(&botapi::InlineButtonAction::CallbackData(hide)));
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
        assert!(html(&screen).contains("42.50 USDT"), "{}", screen.html);
    }

    #[test]
    fn refund_reason_uses_limits() {
        let mut v = view(OrderState::RefundPending);
        v.refund_amount = Some(dec!(1.5));
        v.failure_code = Some("below_minimum".into());
        let extra = Extra {
            min_amount: Some(dec!(2)),
            max_amount: Some(dec!(100)),
            ..Extra::default()
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
        assert!(html(&screen).contains("1.50 USDT"), "{}", screen.html);
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
        assert!(html(&known).contains("100.00 USDT"), "{}", known.html);
        // Макет B1: «Обычно до 30 минут» — срок из настройки, без часов на циферблате.
        assert!(
            html(&known).contains("Обычно до 30 минут"),
            "{}",
            known.html
        );
        let mut v = view(OrderState::ManualReview);
        v.intake_amount = None;
        let unknown = render(&ui, &cfg(), &v, &Extra::default());
        assert_ne!(known.html, unknown.html);
    }
}
