//! «Предпросмотр экранов» (PLAN-v1 1.8, CLAUDE.md п. 21): все экраны клиента в порядке
//! вкладки «Клиент» утверждённого макета (`docs/mockups/template.html`, массив `FLOW`) и на тех
//! же данных — 100.00 → 97.50 USDT, заявка K7M2Q9XR4T, счёт 50.00 → 51.25 USDT, 5 ноября,
//! 14:04 МСК. Бот присылает их владельцу настоящими сообщениями, тест сверяет с текстом
//! макета (`ui/tests/mockup.rs`).
//!
//! Названия экранов — ровно `t` из `FLOW`: по ним тест находит экран в макете.

use chrono::{DateTime, Duration, TimeZone as _, Utc};
use domain::{Asset, Decimal, Direction, Money, OrderState};

use crate::model::{
    Breakdown, DirectionStatus, DirectionTerms, Excess, HistoryRow, HistorySummary, InvoiceQuote,
    InvoiceRefusal, OrderCard, OrderRef, RefundReason, Terms, UiError,
};
use crate::screen::Screen;
use crate::screens::Ui;

/// Номер заявки из макета.
pub const ORDER: &str = "K7M2Q9XR4T";
const CHECK_URL: &str = "https://t.me/xrocket?start=t_val6vkkTbAbxd06";
const REFUND_URL: &str = "https://t.me/xrocket?start=t_r3fUnD51x25";

/// Экран макета: название (`t` из `FLOW`) и сообщения бота по порядку.
#[derive(Debug, Clone)]
pub struct PreviewScreen {
    pub title: &'static str,
    pub messages: Vec<Screen>,
}

fn usdt(v: i64, scale: u32) -> Money {
    Money::new(Decimal::new(v, scale), Asset::Usdt).unwrap_or_else(|_| Money::zero(Asset::Usdt))
}

/// 5 ноября 2026, `h:m` по Москве.
fn msk(day: u32, h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, day, h, m, 0)
        .single()
        .unwrap_or_default()
        - Duration::hours(3)
}

/// «Сейчас» предпросмотра: 5 ноября, 14:05 МСК.
pub fn now() -> DateTime<Utc> {
    msk(5, 14, 5)
}

fn dir(direction: Direction, fee: i64, max: i64, status: DirectionStatus) -> DirectionTerms {
    DirectionTerms {
        direction,
        fee_pct: Decimal::new(fee, 1),
        min_fee: Decimal::new(10, 2),
        min: Decimal::ONE,
        max: Decimal::from(max),
        status,
        tiers: Vec::new(),
        promo: None,
        personal: false,
        pause: None,
        new_user_max: Some(Decimal::ONE_HUNDRED),
    }
}

/// Условия из макета: 2.5 % и 1.5 % на 1–500, счета 2.5 % на 1–300, счета xRocket на паузе.
pub fn terms() -> Terms {
    Terms {
        directions: vec![
            dir(Direction::CbToXrCheck, 25, 500, DirectionStatus::Open),
            dir(Direction::XrToCbCheck, 15, 500, DirectionStatus::Open),
            dir(Direction::PayCbInvoice, 25, 300, DirectionStatus::Open),
            dir(Direction::PayXrInvoice, 25, 300, DirectionStatus::Paused),
        ],
        bind_username: None,
        updated_at: Some(now()),
    }
}

fn order() -> OrderRef {
    OrderRef {
        id: ORDER.to_owned(),
        direction: Direction::CbToXrCheck,
    }
}

fn invoice_order() -> OrderRef {
    OrderRef {
        id: ORDER.to_owned(),
        direction: Direction::PayCbInvoice,
    }
}

fn breakdown() -> Breakdown {
    Breakdown {
        amount_in: usdt(100, 0),
        fee_pct: Decimal::new(25, 1),
        fee: usdt(250, 2),
        payout: usdt(9750, 2),
    }
}

fn quote() -> InvoiceQuote {
    InvoiceQuote {
        order: invoice_order(),
        invoice_amount: usdt(50, 0),
        fiat: None,
        description: None,
        fee_pct: Decimal::new(25, 1),
        fee: usdt(125, 2),
        to_pay: usdt(5125, 2),
        expires_at: msk(5, 14, 25),
    }
}

fn history() -> Vec<HistoryRow> {
    let row = |at, direction, amount: Money, payout: Option<Money>, state| HistoryRow {
        order: ORDER.to_owned(),
        created_at: at,
        direction,
        amount: Some(amount),
        payout,
        state,
    };
    vec![
        row(
            msk(5, 14, 4),
            Direction::CbToXrCheck,
            usdt(100, 0),
            Some(usdt(9750, 2)),
            OrderState::Completed,
        ),
        row(
            msk(4, 18, 20),
            Direction::PayCbInvoice,
            usdt(5125, 2),
            Some(usdt(50, 0)),
            OrderState::Completed,
        ),
        row(
            msk(2, 11, 0),
            Direction::CbToXrCheck,
            usdt(50, 0),
            None,
            OrderState::Refunded,
        ),
        row(
            msk(1, 9, 30),
            Direction::XrToCbCheck,
            usdt(20, 0),
            Some(usdt(1970, 2)),
            OrderState::Completed,
        ),
    ]
}

fn card() -> OrderCard {
    OrderCard {
        order: order(),
        state: OrderState::Completed,
        created_at: msk(5, 14, 4),
        amount_in: Some(usdt(100, 0)),
        fee: Some(usdt(250, 2)),
        payout: Some(usdt(9750, 2)),
        invoice_amount: None,
        refunded: None,
        check_url: Some(CHECK_URL.to_owned()),
        fee_pct: Some(Decimal::new(25, 1)),
        finished_at: Some(msk(5, 14, 4) + Duration::seconds(9)),
    }
}

/// Все экраны клиента в порядке макета.
pub fn client(ui: &Ui) -> Vec<PreviewScreen> {
    let t = terms();
    let o = order();
    let b = breakdown();
    let q = quote();
    let one = |title, screen| PreviewScreen {
        title,
        messages: vec![screen],
    };
    vec![
        one("Приветствие", ui.welcome(&t)),
        one("Обменять → куда", ui.exchange_menu(&t)),
        one("Пришлите чек", ui.send_check(&t, Direction::CbToXrCheck)),
        one(
            "Проверяю",
            ui.order_checking(&o, Some(&usdt(100, 0)), false),
        ),
        one("Отправляю", ui.order_sending(&o, &usdt(9750, 2), false)),
        one(
            "Дольше обычного",
            ui.order_sending(&o, &usdt(9750, 2), true),
        ),
        one("Готово", ui.order_done(&o, &b, CHECK_URL, 9, false)),
        one("Детали", ui.order_done(&o, &b, CHECK_URL, 9, true)),
        one("Курс и лимиты", ui.terms(&t)),
        one("Счёт: к оплате", ui.invoice_quote(&q)),
        one(
            "Счёт: не хватает",
            ui.invoice_underpaid(&q, &usdt(50, 0), &usdt(125, 2)),
        ),
        one(
            "Счёт оплачен",
            ui.invoice_paid(&q, None, None, msk(5, 14, 13)),
        ),
        one(
            "Переплата",
            ui.invoice_paid(
                &InvoiceQuote {
                    order: OrderRef {
                        direction: Direction::PayCbInvoice,
                        ..invoice_order()
                    },
                    ..q.clone()
                },
                Some(&Excess::Refund(usdt(875, 2))),
                Some(REFUND_URL),
                msk(5, 14, 13),
            ),
        ),
        PreviewScreen {
            title: "Возврат",
            messages: vec![
                ui.refund_started(
                    &invoice_order(),
                    &usdt(5125, 2),
                    &RefundReason::InvoicePaidElsewhere,
                ),
                ui.refund_done(&invoice_order(), &usdt(5125, 2), REFUND_URL, None),
            ],
        },
        one(
            "Ручная проверка",
            ui.manual_review(&o, Some(&usdt(100, 0)), 15),
        ),
        one("Мульти-чек xRocket", ui.error(&UiError::MultiCheck)),
        one("Чек уже открыт", ui.error(&UiError::AlreadyActivated)),
        one("Чек не подошёл", ui.error(&UiError::NeedsSubscription)),
        one(
            "Меньше минимума",
            ui.refund_done(
                &o,
                &usdt(50, 2),
                REFUND_URL,
                Some(&RefundReason::BelowMinimum { min: usdt(100, 2) }),
            ),
        ),
        PreviewScreen {
            title: "Лимит и пауза",
            messages: vec![
                ui.error(&UiError::DailyLimit),
                ui.error(&UiError::InvoiceRefused(InvoiceRefusal::Paused)),
            ],
        },
        one(
            "Обслуживание",
            ui.error(&UiError::Maintenance {
                until: Some(msk(5, 15, 30)),
            }),
        ),
        one(
            "История",
            ui.history(
                &history(),
                None,
                Some(&HistorySummary {
                    completed: 23,
                    volume: Decimal::from(2310),
                }),
                now(),
            ),
        ),
        one("Карточка заявки", ui.order_card(&card(), now())),
        PreviewScreen {
            title: "Поддержка",
            messages: vec![ui.support(None), ui.support_sent(Some(ORDER))],
        },
        one("Без ссылки", ui.no_link()),
    ]
}
