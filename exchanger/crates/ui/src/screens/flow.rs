//! Денежный поток одной заявки: прогресс обмена чека, котировка и оплата счёта, ручная
//! проверка, возврат. Обычный HTML: эти сообщения должны показываться в любом клиенте
//! (SPEC §6.1). Исключение — «Квитанция» счёта (выбор владельца 8 ноября 2026): rich и
//! тот же HTML запасом. Одно сообщение на заявку правится по шагам — у всех шагов
//! инлайн-клавиатура или никакой (её можно поставить при `editMessageText`).

use botapi::ButtonStyle;
use chrono::{DateTime, Utc};
use domain::Money;

use super::Ui;
use crate::callback::Callback;
use crate::doc::Doc;
use crate::emoji::Emoji;
use crate::fmt::{self, TimeFormat};
use crate::html::{Frag, clip};
use crate::model::{Breakdown, Excess, InvoiceQuote, OrderRef, RefundReason, UiError};
use crate::screen::{Keyboard, Screen};

/// Сколько символов описания счёта от продавца показываем.
const DESCRIPTION_MAX_CHARS: usize = 200;

impl Ui {
    /// Значок шага: свой, а если шаг затянулся — часы (макет B1, состояние «Долго»).
    fn step_icon(&self, role: Emoji, slow: bool) -> Frag {
        self.emoji.frag(if slow { Emoji::Wait } else { role })
    }

    /// Строка-успокоение под шагом, если он затянулся (макет B1: «Дольше обычного»).
    fn with_slow(&self, mut body: Frag, slow: bool) -> Frag {
        if slow {
            body.push(&Frag::trusted("\n"));
            body.push(&self.fill(&self.texts.order.slow, &[]));
        }
        body
    }

    /// B1 «Проверяю чек на 100.00 USDT» (три точки «в работе»). Сумма неизвестна, пока
    /// кошелёк не открыл чек, — тогда просто «Проверяю чек». Без кнопок: одно окно.
    pub fn order_checking(&self, _order: &OrderRef, amount: Option<&Money>, slow: bool) -> Screen {
        let t = &self.texts.order;
        let icon = self.step_icon(Emoji::Process, slow);
        let body = match amount {
            Some(a) => self.fill(
                &t.checking_amount,
                &[("icon", &icon), ("amount", &self.usdt(a.amount()))],
            ),
            None => self.fill(&t.checking, &[("icon", &icon)]),
        };
        self.plain(self.with_slow(body, slow), Keyboard::None)
    }

    /// B1 «Отправляю 97.50 USDT в xRocket»: деньги клиента у нас, сумма выплаты не меняется.
    pub fn order_sending(&self, order: &OrderRef, payout: &Money, slow: bool) -> Screen {
        let body = self.fill(
            &self.texts.order.sending,
            &[
                ("icon", &self.step_icon(Emoji::Sending, slow)),
                ("payout", &self.usdt(payout.amount())),
                ("dst", &self.wallet(order.direction.target())),
            ],
        );
        self.plain(self.with_slow(body, slow), Keyboard::None)
    }

    /// B1 «Готово»: «97.50 USDT ждут вас в xRocket» + [Забрать] (зелёная, ссылка на чек),
    /// [Детали] [Ещё обмен]. `details` — то же сообщение с разбивкой и номером заявки
    /// (кнопка «Скрыть» сворачивает обратно).
    pub fn order_done(
        &self,
        order: &OrderRef,
        b: &Breakdown,
        check_url: &str,
        took_secs: i64,
        details: bool,
    ) -> Screen {
        let t = &self.texts.order;
        let buttons = &self.texts.buttons;
        let payout = Frag::text(&fmt::amount(b.payout.amount(), b.payout.asset()));
        let dst = self.wallet(order.direction.target());
        let body = if details {
            self.fill(
                &t.details,
                &[
                    ("payout", &payout),
                    ("dst", &dst),
                    (
                        "amount_in",
                        &Frag::text(&fmt::amount(b.amount_in.amount(), b.amount_in.asset())),
                    ),
                    ("fee_pct", &self.pct(b.fee_pct)),
                    (
                        "fee",
                        &Frag::text(&fmt::amount(b.fee.amount(), b.fee.asset())),
                    ),
                    ("order", &Frag::text(&order.id)),
                    ("took", &Frag::text(&fmt::took(took_secs))),
                ],
            )
        } else {
            self.fill(&t.done, &[("payout", &payout), ("dst", &dst)])
        };
        let toggle = if details {
            self.cb_button(
                Some(Emoji::Hide),
                &buttons.hide,
                &Callback::HideDetails(order.id.clone()),
            )
        } else {
            self.cb_button(
                Some(Emoji::Details),
                &buttons.details,
                &Callback::Details(order.id.clone()),
            )
        };
        let keyboard = Keyboard::inline_rows(vec![
            vec![
                self.url_button(Some(Emoji::Claim), &buttons.claim, check_url)
                    .style(ButtonStyle::Success),
            ],
            vec![
                toggle,
                self.cb_button(Some(Emoji::Exchange), &buttons.again, &Callback::Exchange),
            ],
        ]);
        self.plain(body, keyboard)
    }

    /// Квитанция счёта: «Счёт CryptoBot 50.00 · Комиссия 3 % 1.50 · Чек xRocket 51.50».
    /// Комиссия — разница «прислать − счёт», чтобы столбик всегда сходился.
    fn invoice_receipt(&self, q: &InvoiceQuote) -> Frag {
        let r = &self.texts.receipt;
        let d = q.order.direction;
        let invoice = q.invoice_amount.amount();
        let to_pay = q.to_pay.amount();
        self.receipt(
            None,
            &[
                (
                    super::fill_text(&r.invoice, &[("wallet", self.wallet_name(d.target()))]),
                    invoice,
                ),
                (
                    super::fill_text(&r.fee, &[("pct", &fmt::percent(q.fee_pct))]),
                    to_pay - invoice,
                ),
            ],
            (
                &super::fill_text(&r.payout, &[("wallet", self.wallet_name(d.source()))]),
                to_pay,
            ),
        )
    }

    /// «⏳ Цена держится до 21:08 · через 15 минут»: отсчёт `<tg-time format="r">` Telegram
    /// ведёт сам; в обычном HTML он есть, только если там включён `<tg-time>`.
    fn price_holds(&self, q: &InvoiceQuote, now: DateTime<Utc>) -> Frag {
        let t = &self.texts.invoice;
        let minutes = ((q.expires_at - now).num_seconds() + 59)
            .div_euclid(60)
            .max(1);
        let word = &t.minute_forms[fmt::plural_index(minutes)];
        let text = super::fill_text(&t.left, &[("n", &minutes.to_string()), ("word", word)]);
        let tag = format!(
            " · <tg-time unix=\"{}\" format=\"r\">{}</tg-time>",
            q.expires_at.timestamp(),
            crate::html::esc(&text)
        );
        let plain = if self.config.tg_time_in_html {
            tag.clone()
        } else {
            String::new()
        };
        self.fill(
            &t.expires,
            &[
                ("time", &self.at(q.expires_at, TimeFormat::Time)),
                ("left", &Frag::dual(tag, plain)),
            ],
        )
    }

    /// Кнопки квитанции счёта: команда кошелька с суммой (синяя, копируется) и «Отмена».
    fn pay_keyboard(&self, q: &InvoiceQuote) -> Keyboard {
        let command = super::inline_command(q.order.direction.source(), q.to_pay.amount());
        Keyboard::inline_rows(vec![
            vec![self.command_button(&command)],
            vec![self.cb_button(
                Some(Emoji::Hide),
                &self.texts.buttons.cancel,
                &Callback::Cancel(Some(q.order.id.clone())),
            )],
        ])
    }

    /// Счёт в квитанции (макет B1 «Счёт: к оплате», выбор владельца «Квитанция»):
    /// путь денег, описание продавца и фиат, столбик сумм, «Скопируйте, чтобы создать чек ⬇️»,
    /// срок цены; под сообщением — `@xrocket 51.50` и «Отмена».
    pub fn invoice_quote(&self, q: &InvoiceQuote, now: DateTime<Utc>) -> Screen {
        let t = &self.texts.invoice;
        let d = q.order.direction;
        let mut doc = Doc::new();
        doc.para(&self.fill(
            &t.head,
            &[
                ("inv_wallet", &self.wallet(d.target())),
                (
                    "src_icon",
                    &self.emoji.frag(super::wallet_emoji(d.source())),
                ),
                ("src", &self.wallet(d.source())),
            ],
        ));
        if let Some(text) = q
            .description
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            doc.para(&self.fill(
                &t.description,
                &[("text", &Frag::text(&clip(text, DESCRIPTION_MAX_CHARS)))],
            ));
        }
        if let Some(fiat) = &q.fiat {
            doc.para(&self.fill(
                &t.fiat,
                &[
                    (
                        "fiat",
                        &Frag::text(&format!(
                            "{}\u{a0}{}",
                            fiat.amount.normalize(),
                            clip(&fiat.currency, 8)
                        )),
                    ),
                    ("buffer", &self.pct(fiat.buffer_pct)),
                ],
            ));
        }
        doc.block(&self.invoice_receipt(q));
        doc.para(&self.copy_hint());
        doc.footer(&self.price_holds(q, now));
        self.receipt_screen(doc, self.pay_keyboard(q))
    }

    /// «Жду чек xRocket по счёту CryptoBot» — та же квитанция после «чек не подошёл» или
    /// «Пришлю чек» у старых сообщений.
    pub fn invoice_awaiting(&self, q: &InvoiceQuote, now: DateTime<Utc>) -> Screen {
        let d = q.order.direction;
        let mut doc = Doc::new();
        doc.para(&self.fill(
            &self.texts.invoice.awaiting,
            &[
                ("src", &self.wallet(d.source())),
                ("inv_wallet", &self.wallet(d.target())),
            ],
        ));
        doc.block(&self.invoice_receipt(q));
        doc.para(&self.copy_hint());
        doc.footer(&self.price_holds(q, now));
        self.receipt_screen(doc, self.pay_keyboard(q))
    }

    /// «Не хватает 1.50 USDT» — квитанция «К оплате · Пришло · Не хватает», под сообщением
    /// `@xrocket 1.50` (дослать) и «Вернуть 50.00 USDT».
    pub fn invoice_underpaid(&self, q: &InvoiceQuote, got: &Money, rest: &Money) -> Screen {
        let r = &self.texts.receipt;
        let mut doc = Doc::new();
        doc.para(&self.fill(
            &self.texts.invoice.underpaid,
            &[("rest", &self.money(rest))],
        ));
        doc.block(&self.receipt(
            None,
            &[
                (r.to_pay.clone(), q.to_pay.amount()),
                (r.got.clone(), got.amount()),
            ],
            (&r.short, rest.amount()),
        ));
        doc.para(&self.copy_hint());
        let command = super::inline_command(q.order.direction.source(), rest.amount());
        let got_amount = fmt::amount(got.amount(), got.asset());
        let refund_label = self.button_text(
            &self.texts.buttons.refund_amount,
            &[("amount", &got_amount)],
        );
        let keyboard = Keyboard::inline_rows(vec![
            vec![self.command_button(&command)],
            vec![self.cb_button(
                Some(Emoji::Refund),
                &refund_label,
                &Callback::Refund(q.order.id.clone()),
            )],
        ]);
        self.receipt_screen(doc, keyboard)
    }

    /// «Оплачиваю счёт CryptoBot» — шаг, как «Отправляю» у обмена.
    pub fn invoice_paying(&self, q: &InvoiceQuote) -> Screen {
        let body = self.fill(
            &self.texts.invoice.paying,
            &[("inv_wallet", &self.wallet(q.order.direction.target()))],
        );
        self.plain(body, Keyboard::None)
    }

    /// После чека, который кошелёк не принял: что случилось и что заявка ждёт дальше.
    pub fn invoice_check_rejected(
        &self,
        q: &InvoiceQuote,
        error: &UiError,
        now: DateTime<Utc>,
    ) -> Screen {
        let error = self.error(error);
        let awaiting = self.invoice_awaiting(q, now);
        Screen {
            html: format!("{}\n\n{}", error.html, awaiting.html),
            rich_html: None,
            keyboard: awaiting.keyboard,
        }
    }

    /// «Счёт оплачен · 50.00 USDT» — макет B1: детали свёрнуты в цитату; переплата —
    /// «Лишние 8.75 USDT вернул чеком xRocket» и [Забрать 8.75 USDT].
    pub fn invoice_paid(
        &self,
        q: &InvoiceQuote,
        excess: Option<&Excess>,
        excess_check: Option<&str>,
        finished_at: DateTime<Utc>,
    ) -> Screen {
        let t = &self.texts.invoice;
        let src = self.wallet(q.order.direction.source());
        let inv_amount = self.money(&q.invoice_amount);
        let again = || {
            Keyboard::inline_rows(vec![vec![
                self.cb_button(
                    Some(Emoji::Invoice),
                    &self.texts.buttons.another_invoice,
                    &Callback::PayInvoice,
                ),
                self.cb_button(
                    Some(Emoji::History),
                    &self.texts.menu.history,
                    &Callback::History { page: 0 },
                ),
            ]])
        };
        let (body, keyboard) = match excess {
            Some(Excess::Refund(amount)) => {
                let args = [
                    ("inv_amount", &inv_amount),
                    ("excess", &self.money(amount)),
                    ("src", &src),
                ];
                match excess_check {
                    Some(url) => (
                        self.fill(&t.excess_refund, &args),
                        Keyboard::inline_rows(vec![vec![self.claim_button(amount, url)]]),
                    ),
                    None => (self.fill(&t.excess_refund_pending, &args), Keyboard::None),
                }
            }
            Some(Excess::Kept { amount, min_refund }) => (
                self.fill(
                    &t.excess_kept,
                    &[
                        ("inv_amount", &inv_amount),
                        ("excess", &self.money(amount)),
                        ("min_refund", &self.money(min_refund)),
                    ],
                ),
                again(),
            ),
            None => (
                self.fill(
                    &t.paid,
                    &[
                        ("inv_amount", &inv_amount),
                        ("x_short", &self.amount_text(&q.to_pay)),
                        ("fee_pct", &self.pct(q.fee_pct)),
                        ("fee_short", &self.amount_text(&q.fee)),
                        ("order", &Frag::text(&q.order.id)),
                        ("time", &self.at(finished_at, TimeFormat::Time)),
                    ],
                ),
                again(),
            ),
        };
        self.plain(body, keyboard)
    }

    /// «Забрать 51.25 USDT» — зелёная, ссылка на чек.
    pub(super) fn claim_button(&self, amount: &Money, url: &str) -> botapi::InlineKeyboardButton {
        let label = self.button_text(
            &self.texts.refund.claim,
            &[("amount", &fmt::amount(amount.amount(), amount.asset()))],
        );
        self.url_button(Some(Emoji::Claim), &label, url)
            .style(ButtonStyle::Success)
    }

    /// «Проверяю вручную / Деньги у меня: 100.00 USDT. Обычно до 15 минут. / Ничего делать
    /// не нужно — напишу сюда.» — макет B1. `amount` — `None`: исход приёма неизвестен.
    pub fn manual_review(&self, order: &OrderRef, amount: Option<&Money>, minutes: i64) -> Screen {
        let t = &self.texts.manual;
        let minutes = Frag::text(&fmt::minutes_gen(minutes));
        let body = match amount {
            Some(m) => self.fill(
                &t.review,
                &[("amount", &self.money(m)), ("minutes", &minutes)],
            ),
            None => self.fill(&t.review_unknown, &[("minutes", &minutes)]),
        };
        let keyboard = Keyboard::inline_rows(vec![vec![
            self.cb_button(
                Some(Emoji::Support),
                &self.texts.buttons.support,
                &Callback::Support(Some(order.id.clone())),
            ),
            self.copy_button(&order.id, &order.id),
        ]]);
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

    /// Текст возврата: «Меньше минимума · от 1.00 USDT / Верну 0.50 чеком CryptoBot.» или
    /// «Возвращаю 51.25 USDT / Счёт уже оплатил кто-то другой.» — макет B1.
    fn refund_text(&self, order: &OrderRef, refund: &Money, reason: &RefundReason) -> Frag {
        let t = &self.texts.refund;
        let src = self.wallet(order.direction.source());
        match reason {
            RefundReason::BelowMinimum { min } => self.fill(
                &t.below_minimum,
                &[
                    ("min", &self.money(min)),
                    ("refund_short", &self.amount_code(refund)),
                    ("src", &src),
                ],
            ),
            other => self.fill(
                &t.started,
                &[
                    ("refund", &self.money(refund)),
                    ("reason", &self.refund_reason(other)),
                ],
            ),
        }
    }

    /// Возврат начат: почему и сколько. Без кнопок — чек ещё создаётся.
    pub fn refund_started(
        &self,
        order: &OrderRef,
        refund: &Money,
        reason: &RefundReason,
    ) -> Screen {
        self.plain(self.refund_text(order, refund, reason), Keyboard::None)
    }

    /// Возврат готов — [Забрать 51.25 USDT]. Чек меньше минимума — та же строка, что и до
    /// чека, плюс кнопка (макет «Меньше минимума»).
    pub fn refund_done(
        &self,
        order: &OrderRef,
        refund: &Money,
        check_url: &str,
        reason: Option<&RefundReason>,
    ) -> Screen {
        let body = match reason {
            Some(r @ RefundReason::BelowMinimum { .. }) => self.refund_text(order, refund, r),
            _ => self.fill(
                &self.texts.refund.done,
                &[
                    ("refund", &self.money(refund)),
                    ("src", &self.wallet(order.direction.source())),
                ],
            ),
        };
        let keyboard = Keyboard::inline_rows(vec![vec![self.claim_button(refund, check_url)]]);
        self.plain(body, keyboard)
    }
}
