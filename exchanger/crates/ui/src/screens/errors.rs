//! Ошибки и отказы (SPEC §6.3, адаптировано к v0.2: чеки с паролем не принимаем, возврат —
//! чеком). Каждый текст отвечает на два вопроса: где сейчас деньги клиента и что делать.

use super::Ui;
use crate::callback::Callback;
use crate::emoji::Emoji;
use crate::fmt::TimeFormat;
use crate::html::{Frag, clip};
use crate::model::{InvoiceProblem, UiError};
use crate::screen::{Keyboard, Screen};

/// Сколько символов чужого текста (имя бота, причина бана) показываем.
const FOREIGN_MAX_CHARS: usize = 64;
const REASON_MAX_CHARS: usize = 200;

impl Ui {
    fn invoice_problem(&self, problem: InvoiceProblem) -> &str {
        let p = &self.texts.invoice_problems;
        match problem {
            InvoiceProblem::Reusable => &p.reusable,
            InvoiceProblem::AlreadyPaid => &p.already_paid,
            InvoiceProblem::Expired => &p.expired,
            InvoiceProblem::NoAmount => &p.no_amount,
            InvoiceProblem::UnsupportedAsset => &p.unsupported_asset,
            InvoiceProblem::ExpiresSoon => &p.expires_soon,
            InvoiceProblem::Unreadable => &p.unreadable,
        }
    }

    /// Экран ошибки или отказа. Обычный HTML: это часть денежного потока.
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
                Keyboard::None,
            ),
            UiError::ForeignBot { bot } => (
                self.fill(
                    &e.foreign_bot,
                    &[("bot", &Frag::code(&clip(bot, FOREIGN_MAX_CHARS)))],
                ),
                Keyboard::None,
            ),
            UiError::Testnet => (self.fill(&e.testnet, &[]), Keyboard::None),
            UiError::AlreadyActivated => (self.fill(&e.already_activated, &[]), Keyboard::None),
            UiError::NotFound => (self.fill(&e.not_found, &[]), Keyboard::None),
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
                (body, Keyboard::None)
            }
            UiError::PremiumOnly => (self.fill(&e.premium_only, &[]), Keyboard::None),
            UiError::NeedsSubscription => (self.fill(&e.subscription, &[]), Keyboard::None),
            UiError::Captcha => (self.fill(&e.captcha, &[]), Keyboard::None),
            UiError::PasswordProtected => (self.fill(&e.password, &[]), Keyboard::None),
            UiError::MultiCheck => (self.fill(&e.multi_check, &[]), Keyboard::None),
            UiError::BelowMinimum {
                amount_in,
                min,
                source,
            } => (
                self.fill(
                    &e.below_minimum,
                    &[
                        ("amount_in", &self.money(amount_in)),
                        ("min", &self.money(min)),
                        ("src", &self.wallet(*source)),
                    ],
                ),
                Keyboard::None,
            ),
            UiError::AboveMaximum {
                amount_in,
                max,
                source,
            } => (
                self.fill(
                    &e.above_maximum,
                    &[
                        ("amount_in", &self.money(amount_in)),
                        ("max", &self.money(max)),
                        ("src", &self.wallet(*source)),
                    ],
                ),
                Keyboard::None,
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
                let keyboard = Keyboard::inline_rows(vec![vec![self.cb_button(
                    Some(Emoji::Bell),
                    &b.notify,
                    &Callback::Notify(*direction),
                )]]);
                (body, keyboard)
            }
            UiError::NoReserve { max } => (
                self.fill(&e.no_reserve, &[("max", &self.money(max))]),
                Keyboard::None,
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
                (body, Keyboard::None)
            }
            UiError::Duplicate { order } => {
                let keyboard = if crate::callback::order_id_ok(order) {
                    Keyboard::inline_rows(vec![vec![self.cb_button(
                        None,
                        &b.open_order,
                        &Callback::Order(order.clone()),
                    )]])
                } else {
                    Keyboard::None
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
                Keyboard::None,
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
                Keyboard::None,
            ),
            UiError::InvoicePaidElsewhere { to_pay, source } => (
                self.fill(
                    &e.invoice_paid_elsewhere,
                    &[("x", &self.money(to_pay)), ("src", &self.wallet(*source))],
                ),
                Keyboard::None,
            ),
            UiError::RateMoved { to_pay, source } => (
                self.fill(
                    &e.rate_moved,
                    &[("x", &self.money(to_pay)), ("src", &self.wallet(*source))],
                ),
                Keyboard::None,
            ),
            UiError::QuoteExpired => (self.fill(&e.quote_expired, &[]), Keyboard::None),
            UiError::Internal => (self.fill(&e.internal, &[]), Keyboard::None),
        };
        self.plain(body, keyboard)
    }
}
