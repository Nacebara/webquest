//! Ошибки и отказы (SPEC §6.3, адаптировано к v0.2: чеки с паролем не принимаем, возврат —
//! чеком). Каждый текст отвечает на два вопроса: где сейчас деньги клиента и что делать.

use super::Ui;
use crate::callback::Callback;
use crate::emoji::Emoji;
use crate::fmt::TimeFormat;
use crate::html::{Frag, clip};
use crate::model::{InvoiceProblem, InvoiceRefusal, UiError};
use crate::screen::{Keyboard, Screen};
use domain::{Decimal, Direction};

/// Сколько символов чужого текста (имя бота, причина бана) показываем.
const FOREIGN_MAX_CHARS: usize = 64;
const REASON_MAX_CHARS: usize = 200;

/// Кнопка под ошибкой.
#[derive(Debug, Clone, Copy)]
enum ErrKey {
    HowTo,
    Support,
    Rates,
    History,
    PayInvoice,
    OtherInvoice,
}

impl Ui {
    fn invoice_problem(&self, problem: InvoiceProblem) -> &str {
        let p = &self.texts.invoice_problems;
        match problem {
            InvoiceProblem::Fiat => &p.fiat,
            InvoiceProblem::Unavailable => &p.unavailable,
            InvoiceProblem::Reusable => &p.reusable,
            InvoiceProblem::AlreadyPaid => &p.already_paid,
            InvoiceProblem::Expired => &p.expired,
            InvoiceProblem::NoAmount => &p.no_amount,
            InvoiceProblem::UnsupportedAsset => &p.unsupported_asset,
            InvoiceProblem::ExpiresSoon => &p.expires_soon,
            InvoiceProblem::Unreadable => &p.unreadable,
        }
    }

    fn invoice_refusal(&self, refusal: &InvoiceRefusal) -> Frag {
        let r = &self.texts.invoice_refusals;
        match refusal {
            InvoiceRefusal::Paused => self.fill(&r.paused, &[]),
            InvoiceRefusal::Maintenance => self.fill(&r.maintenance, &[]),
            InvoiceRefusal::DailyLimit => self.fill(&r.daily_limit, &[]),
            InvoiceRefusal::NoReserve { max } => {
                self.fill(&r.no_reserve, &[("max", &self.money(max))])
            }
            InvoiceRefusal::BelowMinimum { min } => {
                self.fill(&r.below_minimum, &[("min", &self.money(min))])
            }
            InvoiceRefusal::AboveMaximum { max } => {
                self.fill(&r.above_maximum, &[("max", &self.money(max))])
            }
        }
    }

    /// Кнопки под ошибками (шаг 6.2, макет принят 14.11): следующий шаг по смыслу. Подписи —
    /// как в утверждённых макетах («Как создать чек», «Курс», «История», «Оплатить счёт»).
    fn err_keys(&self, keys: &[ErrKey]) -> Keyboard {
        let b = &self.texts.buttons;
        let m = &self.texts.menu;
        let row = keys
            .iter()
            .map(|k| match k {
                ErrKey::HowTo => self.cb_button(
                    Some(Emoji::Help),
                    &b.howto,
                    &Callback::HowTo(Direction::CbToXrCheck),
                ),
                ErrKey::Support => {
                    self.cb_button(Some(Emoji::Support), &b.support, &Callback::Support(None))
                }
                ErrKey::Rates => self.cb_button(Some(Emoji::Rates), &m.rates, &Callback::Terms),
                ErrKey::History => self.cb_button(
                    Some(Emoji::History),
                    &m.history,
                    &Callback::History { page: 0 },
                ),
                ErrKey::PayInvoice => {
                    self.cb_button(Some(Emoji::Invoice), &m.pay_invoice, &Callback::PayInvoice)
                }
                ErrKey::OtherInvoice => self.cb_button(
                    Some(Emoji::Invoice),
                    &b.other_invoice,
                    &Callback::PayInvoice,
                ),
            })
            .collect();
        Keyboard::inline_rows(vec![row])
    }

    /// Экран ошибки или отказа. Обычный HTML: это часть денежного потока. Под каждой — кнопка
    /// по смыслу; без кнопки — только те, где остаётся подождать: обслуживание (так в
    /// утверждённом макете), «слишком много заявок», «ещё проверяю прошлый чек».
    pub fn error(&self, error: &UiError) -> Screen {
        let e = &self.texts.errors;
        let b = &self.texts.buttons;
        let (body, keyboard) = match error {
            UiError::NotCheckOrInvoice => (
                self.fill(
                    &e.not_check_or_invoice,
                    &[
                        ("cb", &Frag::code("t.me/send?start=CQ…")),
                        ("xr", &Frag::code("t.me/xrocket?start=t_…")),
                    ],
                ),
                self.err_keys(&[ErrKey::HowTo, ErrKey::Support]),
            ),
            UiError::ForeignBot { bot } => (
                self.fill(
                    &e.foreign_bot,
                    &[("bot", &Frag::code(&clip(bot, FOREIGN_MAX_CHARS)))],
                ),
                self.err_keys(&[ErrKey::HowTo, ErrKey::Support]),
            ),
            UiError::Testnet => (
                self.fill(&e.testnet, &[]),
                self.err_keys(&[ErrKey::HowTo, ErrKey::Support]),
            ),
            UiError::AlreadyActivated => (
                self.fill(&e.already_activated, &[]),
                Keyboard::inline_rows(vec![vec![self.cb_button(
                    Some(Emoji::Support),
                    &b.support,
                    &Callback::Support(None),
                )]]),
            ),
            UiError::NotFound => (
                self.fill(&e.not_found, &[]),
                self.err_keys(&[ErrKey::HowTo, ErrKey::Support]),
            ),
            UiError::NotForUs { bind_username } => {
                let name = bind_username
                    .as_deref()
                    .map(|n| n.trim_start_matches('@'))
                    .filter(|n| super::username_ok(n));
                let body = match name {
                    Some(name) => self.fill(
                        &e.not_for_us_bind,
                        &[("username", &Frag::code(&format!("@{name}")))],
                    ),
                    None => self.fill(&e.not_for_us, &[]),
                };
                (body, self.err_keys(&[ErrKey::HowTo]))
            }
            UiError::PremiumOnly => (
                self.fill(&e.premium_only, &[]),
                self.err_keys(&[ErrKey::HowTo, ErrKey::Support]),
            ),
            UiError::NeedsSubscription => (
                self.fill(&e.subscription, &[]),
                self.err_keys(&[ErrKey::HowTo, ErrKey::Support]),
            ),
            UiError::Captcha => (
                self.fill(&e.captcha, &[]),
                self.err_keys(&[ErrKey::HowTo, ErrKey::Support]),
            ),
            UiError::PasswordProtected => (
                self.fill(&e.password, &[]),
                self.err_keys(&[ErrKey::HowTo, ErrKey::Support]),
            ),
            UiError::MultiCheck => {
                // Макет: «Пришлите обычный: @xrocket 100» и та же команда на кнопке-копии.
                let cmd = super::inline_command(domain::Platform::XRocket, Decimal::ONE_HUNDRED);
                (
                    self.fill(&e.multi_check, &[("cmd", &Frag::code(&cmd))]),
                    Keyboard::inline_rows(vec![vec![self.copy_button(&cmd, &cmd)]]),
                )
            }
            // «Если вы сами уже меняли этот чек — итог в сообщении той заявки» → «История».
            UiError::CheckUsed => (
                self.fill(&e.check_used, &[]),
                self.err_keys(&[ErrKey::History]),
            ),
            UiError::BelowMinimum {
                amount_in,
                min,
                source,
            } => (
                self.fill(
                    &e.below_minimum,
                    &[
                        ("amount_short", &self.amount_code(amount_in)),
                        ("min", &self.money(min)),
                        ("src", &self.wallet(*source)),
                    ],
                ),
                self.err_keys(&[ErrKey::Rates]),
            ),
            UiError::AboveMaximum {
                amount_in,
                max,
                source,
            } => (
                self.fill(
                    &e.above_maximum,
                    &[
                        ("amount_short", &self.amount_code(amount_in)),
                        ("max", &self.money(max)),
                        ("src", &self.wallet(*source)),
                    ],
                ),
                self.err_keys(&[ErrKey::Rates]),
            ),
            UiError::DirectionPaused { direction, until } => {
                let name = self.direction_name(*direction);
                let body = match until {
                    Some(at) => self.fill(
                        &e.paused_until,
                        &[
                            ("direction", &name),
                            ("until", &self.at(*at, TimeFormat::Time)),
                        ],
                    ),
                    None => self.fill(&e.paused, &[("direction", &name)]),
                };
                // Макет «Лимит и пауза»: что можно сейчас — [Обменять] [Курс].
                let keyboard = Keyboard::inline_rows(vec![
                    vec![self.cb_button(
                        Some(Emoji::Bell),
                        &b.notify,
                        &Callback::Notify(*direction),
                    )],
                    vec![
                        self.cb_button(
                            Some(Emoji::Exchange),
                            &self.texts.menu.exchange,
                            &Callback::Exchange,
                        ),
                        self.cb_button(
                            Some(Emoji::Rates),
                            &self.texts.menu.rates,
                            &Callback::Terms,
                        ),
                    ],
                ]);
                (body, keyboard)
            }
            UiError::NoReserve { max } => (
                self.fill(&e.no_reserve, &[("max", &self.money(max))]),
                self.err_keys(&[ErrKey::Rates]),
            ),
            UiError::Maintenance { until } => {
                let body = match until {
                    Some(at) => self.fill(
                        &e.maintenance_until,
                        &[("until", &self.at(*at, TimeFormat::Time))],
                    ),
                    None => self.fill(&e.maintenance, &[]),
                };
                (body, Keyboard::None)
            }
            // Владелец 14.11, вариант А: с кнопкой «Курс» — не тупик.
            UiError::DailyLimit => (
                self.fill(&e.daily_limit, &[]),
                self.err_keys(&[ErrKey::Rates]),
            ),
            UiError::RateLimited { retry_after_secs } => (
                self.fill(
                    &e.rate_limited,
                    &[("secs", &Frag::code(&retry_after_secs.to_string()))],
                ),
                Keyboard::None,
            ),
            UiError::Banned { until, reason } => {
                let reason = Frag::text(&clip(reason, REASON_MAX_CHARS));
                let body = match until {
                    Some(at) => self.fill(
                        &e.banned_until,
                        &[
                            ("until", &self.at(*at, TimeFormat::Full)),
                            ("reason", &reason),
                        ],
                    ),
                    None => self.fill(&e.banned, &[("reason", &reason)]),
                };
                (body, self.err_keys(&[ErrKey::Support]))
            }
            UiError::Duplicate { order } => {
                let keyboard = if crate::callback::order_id_ok(order) {
                    Keyboard::inline_rows(vec![vec![self.cb_button(
                        None,
                        &b.open_order,
                        &Callback::Order(order.clone()),
                    )]])
                } else {
                    self.err_keys(&[ErrKey::History])
                };
                (
                    self.fill(&e.duplicate, &[("order", &self.order_id(&clip(order, 32)))]),
                    keyboard,
                )
            }
            UiError::InvoiceUnsuitable(problem) => (
                self.fill(
                    &e.invoice_unsuitable,
                    &[("why", &self.fill(self.invoice_problem(*problem), &[]))],
                ),
                self.err_keys(&[ErrKey::OtherInvoice, ErrKey::Support]),
            ),
            UiError::PayoutDelayed { order, payout } => (
                self.fill(
                    &e.payout_delayed,
                    &[
                        ("dst", &self.wallet(order.direction.target())),
                        ("payout", &self.money(payout)),
                        ("order", &self.order_id(&order.id)),
                    ],
                ),
                Keyboard::inline_rows(vec![vec![self.copy_order_button(&order.id)]]),
            ),
            UiError::UnsupportedAsset { amount_in, source } => (
                self.fill(
                    &e.unsupported_asset,
                    &[
                        ("amount_in", &self.money(amount_in)),
                        ("src", &self.wallet(*source)),
                    ],
                ),
                self.err_keys(&[ErrKey::Support]),
            ),
            UiError::InvoicePaidElsewhere { to_pay, source } => (
                self.fill(
                    &e.invoice_paid_elsewhere,
                    &[("x", &self.money(to_pay)), ("src", &self.wallet(*source))],
                ),
                self.err_keys(&[ErrKey::Support]),
            ),
            UiError::RateMoved { to_pay, source } => (
                self.fill(
                    &e.rate_moved,
                    &[("x", &self.money(to_pay)), ("src", &self.wallet(*source))],
                ),
                self.err_keys(&[ErrKey::PayInvoice]),
            ),
            UiError::InvoiceInWork { order } => (
                self.fill(&e.invoice_in_work, &[("order", &self.order_id(order))]),
                self.err_keys(&[ErrKey::History]),
            ),
            UiError::InvoicePending { order } => (
                self.fill(&e.invoice_pending, &[("order", &self.order_id(order))]),
                self.err_keys(&[ErrKey::History]),
            ),
            UiError::InvoiceCheckBusy => (self.fill(&e.invoice_check_busy, &[]), Keyboard::None),
            UiError::InvoiceRefused(InvoiceRefusal::Paused) => (
                self.fill(&e.invoice_paused, &[]),
                Keyboard::inline_rows(vec![vec![
                    self.cb_button(
                        Some(Emoji::Exchange),
                        &self.texts.menu.exchange,
                        &Callback::Exchange,
                    ),
                    self.cb_button(Some(Emoji::Rates), &self.texts.menu.rates, &Callback::Terms),
                ]]),
            ),
            UiError::InvoiceRefused(refusal) => (
                self.fill(
                    &e.invoice_refused,
                    &[("why", &self.invoice_refusal(refusal))],
                ),
                self.err_keys(&[ErrKey::OtherInvoice, ErrKey::Support]),
            ),
            UiError::QuoteExpired => (
                self.fill(&e.quote_expired, &[]),
                self.err_keys(&[ErrKey::PayInvoice]),
            ),
            UiError::Internal => (
                self.fill(&e.internal, &[]),
                self.err_keys(&[ErrKey::Support]),
            ),
        };
        self.plain(body, keyboard)
    }
}
