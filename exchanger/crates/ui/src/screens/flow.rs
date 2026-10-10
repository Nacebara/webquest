//! Денежный поток одной заявки: прогресс обмена чека, котировка и оплата счёта, ручная
//! проверка, возврат. Только обычный HTML: эти сообщения должны показываться в любом
//! клиенте (SPEC §6.1). Одно сообщение на заявку правится по шагам — у всех шагов
//! инлайн-клавиатура или никакой (её можно поставить при `editMessageText`).

use botapi::ButtonStyle;
use chrono::{DateTime, Utc};
use domain::Money;

use super::Ui;
use crate::callback::Callback;
use crate::fmt::{self, TimeFormat};
use crate::html::{Frag, clip};
use crate::model::{Breakdown, Excess, InvoiceQuote, OrderRef, RefundReason};
use crate::screen::{Keyboard, Screen};

/// Сколько символов описания счёта от продавца показываем.
const DESCRIPTION_MAX_CHARS: usize = 200;

impl Ui {
    fn order_keyboard(&self, order: &OrderRef) -> Keyboard {
        Keyboard::inline_rows(vec![vec![self.copy_order_button(&order.id)]])
    }

    /// S3 «Проверяю чек»: активация юзерботом идёт.
    pub fn order_checking(&self, order: &OrderRef) -> Screen {
        let body = self.fill(
            &self.texts.order.checking,
            &[
                ("order", &self.order_id(&order.id)),
                ("src", &self.wallet(order.direction.source())),
                ("dst", &self.wallet(order.direction.target())),
            ],
        );
        self.plain(body, self.order_keyboard(order))
    }

    /// S3 «Чек принят»: деньги у нас, расчёт выплаты.
    pub fn order_accepted(&self, order: &OrderRef, b: &Breakdown) -> Screen {
        let body = self.fill(
            &self.texts.order.accepted,
            &[
                ("amount_in", &self.money(&b.amount_in)),
                ("fee_pct", &self.pct(b.fee_pct)),
                ("fee", &self.money(&b.fee)),
                ("payout", &self.money(&b.payout)),
                ("dst", &self.wallet(order.direction.target())),
            ],
        );
        self.plain(body, self.order_keyboard(order))
    }

    /// S3 «Создаю чек выплаты».
    pub fn order_creating_payout(&self, order: &OrderRef, payout: &Money) -> Screen {
        let body = self.fill(
            &self.texts.order.creating,
            &[
                ("order", &self.order_id(&order.id)),
                ("payout", &self.money(payout)),
                ("dst", &self.wallet(order.direction.target())),
            ],
        );
        self.plain(body, self.order_keyboard(order))
    }

    /// S3 итог: «Ваш чек xRocket на 97.50 USDT» + «Забрать чек» (url, `success`) и
    /// «Скопировать ссылку» (`copy_text`) — DESIGN-v0.2 §6.
    pub fn order_done(
        &self,
        order: &OrderRef,
        b: &Breakdown,
        check_url: &str,
        finished_at: DateTime<Utc>,
    ) -> Screen {
        let buttons = &self.texts.buttons;
        let body = self.fill(
            &self.texts.order.done,
            &[
                ("dst", &self.wallet(order.direction.target())),
                ("payout", &self.money(&b.payout)),
                ("claim", &Frag::text(&buttons.claim_check)),
                ("amount_in", &self.money(&b.amount_in)),
                ("fee_pct", &self.pct(b.fee_pct)),
                ("fee", &self.money(&b.fee)),
                ("order", &self.order_id(&order.id)),
                ("time", &self.at(finished_at, TimeFormat::Time)),
            ],
        );
        let keyboard = Keyboard::inline_rows(vec![
            self.claim_row(check_url),
            vec![self.cb_button(None, &buttons.again, &Callback::Exchange)],
        ]);
        self.plain(body, keyboard)
    }

    fn quote_parts(&self, q: &InvoiceQuote) -> QuoteParts {
        QuoteParts {
            inv_wallet: self.wallet(q.order.direction.target()),
            src: self.wallet(q.order.direction.source()),
            x: self.money(&q.to_pay),
            expires: self.at(q.expires_at, TimeFormat::Time),
        }
    }

    /// S5 котировка счёта: что прислать, разбивка, срок цены.
    pub fn invoice_quote(&self, q: &InvoiceQuote) -> Screen {
        let t = &self.texts.invoice;
        let p = self.quote_parts(q);
        let description = match q.description.as_deref().map(str::trim) {
            Some(text) if !text.is_empty() => self.fill(
                &t.description,
                &[("text", &Frag::text(&clip(text, DESCRIPTION_MAX_CHARS)))],
            ),
            _ => Frag::new(),
        };
        let fiat_line = match &q.fiat {
            Some(fiat) => self.fill(
                &t.fiat_line,
                &[
                    (
                        "fiat",
                        &Frag::code(&format!(
                            "{} {}",
                            fiat.amount.normalize(),
                            clip(&fiat.currency, 8)
                        )),
                    ),
                    ("inv_wallet", &p.inv_wallet),
                    ("buffer", &self.pct(fiat.buffer_pct)),
                ],
            ),
            None => Frag::new(),
        };
        let body = self.fill(
            &t.quote,
            &[
                ("inv_wallet", &p.inv_wallet),
                ("src", &p.src),
                ("x", &p.x),
                ("expires", &p.expires),
                ("inv_amount", &self.money(&q.invoice_amount)),
                ("description", &description),
                ("fiat_line", &fiat_line),
                ("fee_pct", &self.pct(q.fee_pct)),
                ("fee", &self.money(&q.fee)),
            ],
        );
        let b = &self.texts.buttons;
        let id = &q.order.id;
        let pay_label = self.button_text(
            &b.pay_by_check,
            &[("wallet", self.wallet_name(q.order.direction.source()))],
        );
        let keyboard = Keyboard::inline_rows(vec![
            vec![
                self.cb_button(None, &pay_label, &Callback::PayByCheck(id.clone()))
                    .style(ButtonStyle::Primary),
            ],
            vec![
                self.copy_amount_button(&q.to_pay),
                self.cb_button(None, &b.cancel, &Callback::Cancel(Some(id.clone()))),
            ],
        ]);
        self.plain(body, keyboard)
    }

    fn copy_amount_button(&self, m: &Money) -> botapi::InlineKeyboardButton {
        self.copy_button(
            &self.texts.buttons.copy_amount,
            &fmt::amount(m.amount(), m.asset()),
        )
    }

    /// S6 жду чек на точную сумму.
    pub fn invoice_awaiting(&self, q: &InvoiceQuote) -> Screen {
        let p = self.quote_parts(q);
        let body = self.fill(
            &self.texts.invoice.awaiting,
            &[("src", &p.src), ("x", &p.x), ("expires", &p.expires)],
        );
        let b = &self.texts.buttons;
        let id = &q.order.id;
        let keyboard = Keyboard::inline_rows(vec![
            vec![
                self.copy_amount_button(&q.to_pay),
                self.cb_button(None, &b.howto, &Callback::HowTo(q.order.direction)),
            ],
            vec![self.cb_button(None, &b.cancel, &Callback::Cancel(Some(id.clone())))],
        ]);
        self.plain(body, keyboard)
    }

    /// S6 недоплата: дослать остаток или вернуть пришедшее.
    pub fn invoice_underpaid(&self, q: &InvoiceQuote, got: &Money, rest: &Money) -> Screen {
        let p = self.quote_parts(q);
        let body = self.fill(
            &self.texts.invoice.underpaid,
            &[
                ("got", &self.money(got)),
                ("rest", &self.money(rest)),
                ("x", &p.x),
                ("expires", &p.expires),
            ],
        );
        let rest_amount = fmt::amount(rest.amount(), rest.asset());
        let keyboard = Keyboard::inline_rows(vec![vec![
            self.copy_button(&rest_amount, &rest_amount),
            self.cb_button(
                None,
                &self.texts.buttons.refund,
                &Callback::Refund(q.order.id.clone()),
            )
            .style(ButtonStyle::Danger),
        ]]);
        self.plain(body, keyboard)
    }

    /// S7 «Чек принят, оплачиваю счёт».
    pub fn invoice_paying(&self, q: &InvoiceQuote) -> Screen {
        let p = self.quote_parts(q);
        let body = self.fill(
            &self.texts.invoice.paying,
            &[
                ("inv_wallet", &p.inv_wallet),
                ("order", &self.order_id(&q.order.id)),
            ],
        );
        self.plain(body, self.order_keyboard(&q.order))
    }

    /// S7 «Счёт оплачен» и судьба переплаты.
    pub fn invoice_paid(
        &self,
        q: &InvoiceQuote,
        excess: Option<&Excess>,
        finished_at: DateTime<Utc>,
    ) -> Screen {
        let t = &self.texts.invoice;
        let p = self.quote_parts(q);
        let excess_line = match excess {
            Some(Excess::Refund(amount)) => self.fill(
                &t.excess_refund,
                &[("excess", &self.money(amount)), ("src", &p.src)],
            ),
            Some(Excess::Kept { amount, min_refund }) => self.fill(
                &t.excess_kept,
                &[
                    ("excess", &self.money(amount)),
                    ("min_refund", &self.money(min_refund)),
                ],
            ),
            None => Frag::new(),
        };
        let body = self.fill(
            &t.paid,
            &[
                ("inv_amount", &self.money(&q.invoice_amount)),
                ("x", &p.x),
                ("fee", &self.money(&q.fee)),
                ("excess_line", &excess_line),
                ("order", &self.order_id(&q.order.id)),
                ("time", &self.at(finished_at, TimeFormat::Time)),
            ],
        );
        self.plain(body, self.order_keyboard(&q.order))
    }

    /// S8 ручная проверка. `amount` — сколько у нас денег клиента; `None` — исход приёма
    /// неизвестен (чек мог остаться у клиента), текст говорит об обоих вариантах.
    pub fn manual_review(
        &self,
        order: &OrderRef,
        amount: Option<&Money>,
        sla: DateTime<Utc>,
    ) -> Screen {
        let t = &self.texts.manual;
        let order_id = self.order_id(&order.id);
        let sla = self.at(sla, TimeFormat::Time);
        let body = match amount {
            Some(m) => self.fill(
                &t.review,
                &[
                    ("order", &order_id),
                    ("amount", &self.money(m)),
                    ("sla", &sla),
                ],
            ),
            None => self.fill(&t.review_unknown, &[("order", &order_id), ("sla", &sla)]),
        };
        let keyboard = Keyboard::inline_rows(vec![vec![self.cb_button(
            Some(crate::emoji::Emoji::Support),
            &self.texts.buttons.support,
            &Callback::Support(Some(order.id.clone())),
        )]]);
        self.plain(body, keyboard)
    }

    fn refund_reason(&self, reason: &RefundReason) -> Frag {
        let r = &self.texts.refund_reasons;
        match reason {
            RefundReason::BelowMinimum { min } => {
                self.fill(&r.below_minimum, &[("min", &self.money(min))])
            }
            RefundReason::AboveMaximum { max } => {
                self.fill(&r.above_maximum, &[("max", &self.money(max))])
            }
            RefundReason::UnsupportedAsset => self.fill(&r.unsupported_asset, &[]),
            RefundReason::InvoicePaidElsewhere => self.fill(&r.invoice_paid_elsewhere, &[]),
            RefundReason::InvoiceExpired => self.fill(&r.invoice_expired, &[]),
            RefundReason::RateMoved => self.fill(&r.rate_moved, &[]),
            RefundReason::ClientRequest => self.fill(&r.client_request, &[]),
            RefundReason::Underpaid => self.fill(&r.underpaid, &[]),
            RefundReason::Operator => self.fill(&r.operator, &[]),
        }
    }

    /// S9 «Возвращаю … чеком».
    pub fn refund_started(
        &self,
        order: &OrderRef,
        refund: &Money,
        reason: &RefundReason,
    ) -> Screen {
        let body = self.fill(
            &self.texts.refund.started,
            &[
                ("refund", &self.money(refund)),
                ("src", &self.wallet(order.direction.source())),
                ("reason", &self.refund_reason(reason)),
                ("order", &self.order_id(&order.id)),
            ],
        );
        self.plain(body, self.order_keyboard(order))
    }

    /// S9 итог: чек возврата с кнопками «Забрать чек» и «Скопировать ссылку».
    pub fn refund_done(&self, order: &OrderRef, refund: &Money, check_url: &str) -> Screen {
        let body = self.fill(
            &self.texts.refund.done,
            &[
                ("refund", &self.money(refund)),
                ("src", &self.wallet(order.direction.source())),
                ("claim", &Frag::text(&self.texts.buttons.claim_check)),
                ("order", &self.order_id(&order.id)),
            ],
        );
        self.plain(body, Keyboard::inline_rows(vec![self.claim_row(check_url)]))
    }
}

/// Общие подстановки экранов счёта.
struct QuoteParts {
    inv_wallet: Frag,
    src: Frag,
    x: Frag,
    expires: Frag,
}
