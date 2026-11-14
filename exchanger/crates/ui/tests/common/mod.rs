//! Общие данные тестов: два вида рендера (обычный и Premium) и галерея всех экранов.
#![allow(dead_code, clippy::unwrap_used)]

use chrono::{DateTime, TimeZone, Utc};
use domain::{Asset, Decimal, Direction, Money, OrderState, Platform};
use rust_decimal_macros::dec;
use ui::{
    Breakdown, DirectionStatus, DirectionTerms, Emoji, EmojiSet, Excess, FiatAmount, HistoryRow,
    InvestHome, InvestMoveKind, InvestMoveRow, InvestMoveStatus, InvestNotice, InvestProblem,
    InvoiceProblem, InvoiceQuote, InvoiceRefusal, OrderCard, OrderRef, RefundReason, Screen, Terms,
    Ui, UiConfig, UiError,
};

pub const ORDER: &str = "A1B2C3D4E5";
pub const CHECK_URL: &str = "https://t.me/xrocket?start=t_val6vkkTbAbxd06";
pub const REFUND_URL: &str = "https://t.me/send?start=CQ5mN8pQ2rT7";

pub fn plain_ui() -> Ui {
    Ui::builtin(EmojiSet::plain(), UiConfig::default()).unwrap()
}

/// Premium: id для каждой роли — `53680000000000000NN`.
pub fn premium_ui() -> Ui {
    let ids = Emoji::ALL
        .iter()
        .enumerate()
        .map(|(i, role)| (*role, format!("53680000000000000{i:02}")));
    Ui::builtin(EmojiSet::premium(ids).unwrap(), UiConfig::default()).unwrap()
}

pub fn modes() -> [(&'static str, Ui); 2] {
    [("plain", plain_ui()), ("premium", premium_ui())]
}

pub fn at(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 7, h, m, 0).unwrap()
}

pub fn usdt(v: Decimal) -> Money {
    Money::new(v, Asset::Usdt).unwrap()
}

fn dir(
    direction: Direction,
    fee: Decimal,
    min_fee: Decimal,
    max: Decimal,
    status: DirectionStatus,
) -> DirectionTerms {
    DirectionTerms {
        direction,
        fee_pct: fee,
        min_fee,
        min: dec!(2),
        max,
        status,
        tiers: Vec::new(),
        promo: None,
        personal: false,
        pause: (status == DirectionStatus::Paused).then_some(ui::PauseReason::NoReserve {
            wallet: direction.target(),
        }),
        new_user_max: Some(dec!(100)),
    }
}

pub fn terms() -> Terms {
    Terms {
        directions: vec![
            dir(
                Direction::CbToXrCheck,
                dec!(2.5),
                dec!(0.10),
                dec!(500),
                DirectionStatus::Open,
            ),
            dir(
                Direction::XrToCbCheck,
                dec!(1.5),
                dec!(0.10),
                dec!(300),
                DirectionStatus::Open,
            ),
            dir(
                Direction::PayCbInvoice,
                dec!(3),
                dec!(0.20),
                dec!(200),
                DirectionStatus::Open,
            ),
            dir(
                Direction::PayXrInvoice,
                dec!(2),
                dec!(0.20),
                dec!(200),
                DirectionStatus::Paused,
            ),
        ],
        bind_username: Some("exch_cb".to_owned()),
        updated_at: Some(at(14, 5)),
    }
}

/// Встречное направление на паузе, счёт xRocket выключен.
pub fn terms_partly_paused() -> Terms {
    let mut t = terms();
    t.directions[1].status = DirectionStatus::Paused;
    t.directions[1].pause = Some(ui::PauseReason::NoReserve {
        wallet: Platform::CryptoBot,
    });
    t.directions[3].status = DirectionStatus::Off;
    t.bind_username = None;
    t
}

/// Ступени на основном направлении, выгодный курс встречного потока, личные условия.
pub fn terms_with_offers() -> Terms {
    let mut t = terms();
    t.directions[0].tiers = vec![
        ui::TierTerms {
            from: dec!(2),
            fee_pct: dec!(2.5),
        },
        ui::TierTerms {
            from: dec!(100),
            fee_pct: dec!(1.8),
        },
        ui::TierTerms {
            from: dec!(250),
            fee_pct: dec!(1.2),
        },
    ];
    t.directions[1].promo = Some(ui::PromoTerms {
        fee_pct: dec!(0.3),
        up_to: dec!(450),
    });
    t.directions[1].personal = true;
    t
}

pub fn terms_all_paused() -> Terms {
    let mut t = terms();
    for d in &mut t.directions {
        d.status = DirectionStatus::Paused;
    }
    t
}

pub fn order(direction: Direction) -> OrderRef {
    OrderRef {
        id: ORDER.to_owned(),
        direction,
    }
}

/// 100 USDT при 2,5 %: выплата 97.50 (SPEC §2.3).
pub fn breakdown() -> Breakdown {
    Breakdown {
        amount_in: usdt(dec!(100)),
        fee_pct: dec!(2.5),
        fee: usdt(dec!(2.5)),
        payout: usdt(dec!(97.5)),
    }
}

pub fn quote(description: Option<&str>, fiat: bool) -> InvoiceQuote {
    InvoiceQuote {
        order: order(Direction::PayCbInvoice),
        invoice_amount: usdt(dec!(10)),
        fiat: fiat.then(|| FiatAmount {
            amount: dec!(950),
            currency: "RUB".to_owned(),
            buffer_pct: dec!(1),
        }),
        description: description.map(str::to_owned),
        fee_pct: dec!(3),
        fee: usdt(dec!(0.3)),
        to_pay: usdt(dec!(10.3)),
        expires_at: at(12, 49),
    }
}

pub fn history_rows() -> Vec<HistoryRow> {
    let states = [
        (
            Direction::CbToXrCheck,
            Some(dec!(100)),
            OrderState::Completed,
        ),
        (
            Direction::XrToCbCheck,
            Some(dec!(25.5)),
            OrderState::Refunded,
        ),
        (
            Direction::PayCbInvoice,
            Some(dec!(10.3)),
            OrderState::InvoicePayPending,
        ),
        (Direction::CbToXrCheck, None, OrderState::IntakeFailed),
        (
            Direction::CbToXrCheck,
            Some(dec!(7)),
            OrderState::ManualReview,
        ),
    ];
    states
        .into_iter()
        .enumerate()
        .map(|(i, (direction, amount, state))| HistoryRow {
            order: format!("ORD{i:07}"),
            created_at: at(9 + u32::try_from(i).unwrap(), 15),
            direction,
            amount: amount.map(usdt),
            // Итог: выплата = пришло − 2.5 % (для истории важна только сама сумма).
            payout: (state == OrderState::Completed)
                .then(|| amount.map(|a| usdt((a * dec!(0.975)).round_dp(2))))
                .flatten(),
            state,
        })
        .collect()
}

/// «Всего 23 обмена · 2 310 USDT».
pub fn history_summary() -> ui::HistorySummary {
    ui::HistorySummary {
        completed: 23,
        volume: dec!(2310.40),
    }
}

pub fn card(with_check: bool) -> OrderCard {
    OrderCard {
        order: order(Direction::CbToXrCheck),
        state: if with_check {
            OrderState::Completed
        } else {
            OrderState::ManualReview
        },
        created_at: at(12, 30),
        amount_in: Some(usdt(dec!(100))),
        fee: Some(usdt(dec!(2.5))),
        payout: with_check.then(|| usdt(dec!(97.5))),
        invoice_amount: None,
        refunded: None,
        check_url: with_check.then(|| CHECK_URL.to_owned()),
        fee_pct: Some(dec!(2.5)),
        finished_at: with_check.then(|| at(12, 30) + chrono::Duration::seconds(9)),
    }
}

pub fn all_errors() -> Vec<(&'static str, UiError)> {
    vec![
        ("not_check_or_invoice", UiError::NotCheckOrInvoice),
        (
            "foreign_bot",
            UiError::ForeignBot {
                bot: "CryptoBotX <fake> & co".to_owned(),
            },
        ),
        ("testnet", UiError::Testnet),
        ("already_activated", UiError::AlreadyActivated),
        ("not_found", UiError::NotFound),
        (
            "not_for_us",
            UiError::NotForUs {
                bind_username: None,
            },
        ),
        (
            "not_for_us_bind",
            UiError::NotForUs {
                bind_username: Some("@exch_cb".to_owned()),
            },
        ),
        ("premium_only", UiError::PremiumOnly),
        ("subscription", UiError::NeedsSubscription),
        ("captcha", UiError::Captcha),
        ("password", UiError::PasswordProtected),
        ("multi_check", UiError::MultiCheck),
        (
            "below_minimum",
            UiError::BelowMinimum {
                amount_in: usdt(dec!(1.5)),
                min: usdt(dec!(2)),
                source: Platform::CryptoBot,
            },
        ),
        (
            "above_maximum",
            UiError::AboveMaximum {
                amount_in: usdt(dec!(750)),
                max: usdt(dec!(500)),
                source: Platform::CryptoBot,
            },
        ),
        (
            "paused",
            UiError::DirectionPaused {
                direction: Direction::CbToXrCheck,
                until: None,
            },
        ),
        (
            "paused_until",
            UiError::DirectionPaused {
                direction: Direction::XrToCbCheck,
                until: Some(at(14, 0)),
            },
        ),
        (
            "no_reserve",
            UiError::NoReserve {
                max: usdt(dec!(120)),
            },
        ),
        ("maintenance", UiError::Maintenance { until: None }),
        (
            "maintenance_until",
            UiError::Maintenance {
                until: Some(at(13, 30)),
            },
        ),
        ("daily_limit", UiError::DailyLimit),
        (
            "rate_limited",
            UiError::RateLimited {
                retry_after_secs: 42,
            },
        ),
        (
            "banned",
            UiError::Banned {
                until: None,
                reason: "серия неудачных чеков".to_owned(),
            },
        ),
        (
            "banned_until",
            UiError::Banned {
                until: Some(at(13, 0)),
                reason: "серия неудачных чеков".to_owned(),
            },
        ),
        (
            "duplicate",
            UiError::Duplicate {
                order: ORDER.to_owned(),
            },
        ),
        (
            "invoice_reusable",
            UiError::InvoiceUnsuitable(InvoiceProblem::Reusable),
        ),
        (
            "invoice_already_paid",
            UiError::InvoiceUnsuitable(InvoiceProblem::AlreadyPaid),
        ),
        (
            "invoice_expired",
            UiError::InvoiceUnsuitable(InvoiceProblem::Expired),
        ),
        (
            "invoice_no_amount",
            UiError::InvoiceUnsuitable(InvoiceProblem::NoAmount),
        ),
        (
            "invoice_not_usdt",
            UiError::InvoiceUnsuitable(InvoiceProblem::UnsupportedAsset),
        ),
        (
            "invoice_expires_soon",
            UiError::InvoiceUnsuitable(InvoiceProblem::ExpiresSoon),
        ),
        (
            "invoice_unreadable",
            UiError::InvoiceUnsuitable(InvoiceProblem::Unreadable),
        ),
        (
            "payout_delayed",
            UiError::PayoutDelayed {
                order: order(Direction::CbToXrCheck),
                payout: usdt(dec!(97.5)),
            },
        ),
        (
            "unsupported_asset",
            UiError::UnsupportedAsset {
                amount_in: Money::new(dec!(1.5), Asset::Ton).unwrap(),
                source: Platform::XRocket,
            },
        ),
        (
            "invoice_paid_elsewhere",
            UiError::InvoicePaidElsewhere {
                to_pay: usdt(dec!(10.3)),
                source: Platform::XRocket,
            },
        ),
        (
            "rate_moved",
            UiError::RateMoved {
                to_pay: usdt(dec!(10.3)),
                source: Platform::XRocket,
            },
        ),
        (
            "invoice_fiat",
            UiError::InvoiceUnsuitable(InvoiceProblem::Fiat),
        ),
        (
            "invoice_unavailable",
            UiError::InvoiceUnsuitable(InvoiceProblem::Unavailable),
        ),
        (
            "invoice_in_work",
            UiError::InvoiceInWork {
                order: ORDER.to_owned(),
            },
        ),
        (
            "invoice_pending",
            UiError::InvoicePending {
                order: ORDER.to_owned(),
            },
        ),
        ("invoice_check_busy", UiError::InvoiceCheckBusy),
        ("check_used", UiError::CheckUsed),
        (
            "invoice_refused_paused",
            UiError::InvoiceRefused(InvoiceRefusal::Paused),
        ),
        (
            "invoice_refused_maintenance",
            UiError::InvoiceRefused(InvoiceRefusal::Maintenance),
        ),
        (
            "invoice_refused_daily_limit",
            UiError::InvoiceRefused(InvoiceRefusal::DailyLimit),
        ),
        (
            "invoice_refused_no_reserve",
            UiError::InvoiceRefused(InvoiceRefusal::NoReserve {
                max: usdt(dec!(55)),
            }),
        ),
        (
            "invoice_refused_below_minimum",
            UiError::InvoiceRefused(InvoiceRefusal::BelowMinimum { min: usdt(dec!(2)) }),
        ),
        (
            "invoice_refused_above_maximum",
            UiError::InvoiceRefused(InvoiceRefusal::AboveMaximum {
                max: usdt(dec!(100)),
            }),
        ),
        ("quote_expired", UiError::QuoteExpired),
        ("internal", UiError::Internal),
    ]
}

pub fn all_refund_reasons() -> Vec<(&'static str, RefundReason)> {
    vec![
        (
            "below_minimum",
            RefundReason::BelowMinimum { min: usdt(dec!(2)) },
        ),
        (
            "above_maximum",
            RefundReason::AboveMaximum {
                max: usdt(dec!(500)),
            },
        ),
        ("unsupported_asset", RefundReason::UnsupportedAsset),
        ("invoice_paid_elsewhere", RefundReason::InvoicePaidElsewhere),
        ("invoice_expired", RefundReason::InvoiceExpired),
        ("rate_moved", RefundReason::RateMoved),
        ("client_request", RefundReason::ClientRequest),
        ("underpaid", RefundReason::Underpaid),
        ("operator", RefundReason::Operator),
    ]
}

/// Все экраны клиента одним списком. Ошибки и причины возврата — с префиксами.
pub fn gallery(ui: &Ui) -> Vec<(String, Screen)> {
    let t = terms();
    let cb = Direction::CbToXrCheck;
    let mut screens: Vec<(&str, Screen)> = vec![
        ("welcome", ui.welcome(&t)),
        ("welcome_all_paused", ui.welcome(&terms_all_paused())),
        ("terms", ui.terms(&t)),
        ("terms_partly_paused", ui.terms(&terms_partly_paused())),
        ("exchange_menu", ui.exchange_menu(&t)),
        ("terms_with_offers", ui.terms(&terms_with_offers())),
        (
            "exchange_menu_with_offers",
            ui.exchange_menu(&terms_with_offers()),
        ),
        ("welcome_with_offers", ui.welcome(&terms_with_offers())),
        (
            "exchange_menu_paused",
            ui.exchange_menu(&terms_partly_paused()),
        ),
        ("send_check_cb_to_xr", ui.send_check(&t, cb)),
        (
            "promo_broadcast",
            ui.promo_broadcast(&ui::preview::terms_promo(), cb)
                .expect("выгодный курс горит"),
        ),
        (
            "send_check_xr_to_cb",
            ui.send_check(&t, Direction::XrToCbCheck),
        ),
        (
            "send_check_paused",
            ui.send_check(&terms_partly_paused(), Direction::XrToCbCheck),
        ),
        ("howto_cryptobot", ui.check_howto(&t, cb)),
        ("howto_xrocket", ui.check_howto(&t, Direction::XrToCbCheck)),
        ("invoice_prompt", ui.invoice_prompt(&t)),
        // Ход обмена — макет B1: одна строка на шаг, итог — «… ждут вас в xRocket».
        ("order_checking", ui.order_checking(&order(cb), None, false)),
        (
            "order_checking_amount",
            ui.order_checking(&order(cb), Some(&usdt(dec!(100))), false),
        ),
        (
            "order_sending",
            ui.order_sending(&order(cb), &usdt(dec!(97.5)), false),
        ),
        (
            "order_sending_slow",
            ui.order_sending(&order(cb), &usdt(dec!(97.5)), true),
        ),
        (
            "order_done",
            ui.order_done(&order(cb), &breakdown(), CHECK_URL, 9, false),
        ),
        (
            "order_done_details",
            ui.order_done(&order(cb), &breakdown(), CHECK_URL, 9, true),
        ),
        (
            "invoice_quote",
            ui.invoice_quote(
                &quote(Some("Оплата заказа #42 <VIP> & \"скидка\""), false),
                at(12, 34),
            ),
        ),
        (
            "invoice_quote_fiat",
            ui.invoice_quote(&quote(None, true), at(12, 34)),
        ),
        (
            "invoice_awaiting",
            ui.invoice_awaiting(&quote(None, false), at(12, 34)),
        ),
        (
            "invoice_underpaid",
            ui.invoice_underpaid(&quote(None, false), &usdt(dec!(8)), &usdt(dec!(2.3))),
        ),
        ("invoice_paying", ui.invoice_paying(&quote(None, false))),
        (
            "invoice_paid",
            ui.invoice_paid(&quote(None, false), None, None, at(12, 40)),
        ),
        (
            "invoice_paid_excess_refund",
            ui.invoice_paid(
                &quote(None, false),
                Some(&Excess::Refund(usdt(dec!(5)))),
                None,
                at(12, 40),
            ),
        ),
        (
            "invoice_paid_excess_check",
            ui.invoice_paid(
                &quote(None, false),
                Some(&Excess::Refund(usdt(dec!(5)))),
                Some(CHECK_URL),
                at(12, 40),
            ),
        ),
        (
            "invoice_check_rejected",
            ui.invoice_check_rejected(&quote(None, false), &UiError::AlreadyActivated, at(12, 34)),
        ),
        (
            "invoice_paid_excess_kept",
            ui.invoice_paid(
                &quote(None, false),
                Some(&Excess::Kept {
                    amount: usdt(dec!(0.4)),
                    min_refund: usdt(dec!(1)),
                }),
                None,
                at(12, 40),
            ),
        ),
        (
            "manual_review",
            ui.manual_review(&order(cb), Some(&usdt(dec!(100))), 15),
        ),
        // Шаг 6.1: итог ушёл новым сообщением — прежнее сообщение заявки.
        (
            "result_below",
            ui.result_below(
                &ui.manual_review(&order(cb), Some(&usdt(dec!(100))), 15),
                Some(&usdt(dec!(100))),
            ),
        ),
        (
            "manual_review_unknown",
            ui.manual_review(&order(cb), None, 15),
        ),
        (
            "refund_done",
            ui.refund_done(&order(cb), &usdt(dec!(750)), REFUND_URL, None),
        ),
        (
            "refund_done_below_minimum",
            ui.refund_done(
                &order(cb),
                &usdt(dec!(0.5)),
                REFUND_URL,
                Some(&RefundReason::BelowMinimum { min: usdt(dec!(1)) }),
            ),
        ),
        (
            "history",
            ui.history(
                &history_rows(),
                &ui::HistoryPage {
                    page: 0,
                    pages: 2,
                    links: false,
                },
                Some(&history_summary()),
                at(18, 0),
            ),
        ),
        (
            "history_links_page_2",
            ui.history(
                &history_rows(),
                &ui::HistoryPage {
                    page: 1,
                    pages: 3,
                    links: true,
                },
                Some(&history_summary()),
                at(18, 0),
            ),
        ),
        (
            "history_empty",
            ui.history(&[], &ui::HistoryPage::default(), None, at(18, 0)),
        ),
        ("order_card", ui.order_card(&card(false), at(18, 0))),
        (
            "order_card_with_check",
            ui.order_card(&card(true), at(18, 0)),
        ),
        ("help", ui.help(&t)),
        ("privacy", ui.privacy()),
        ("support_with_contact", ui.support(Some("@exch_support"))),
        ("support_without_contact", ui.support(None)),
        ("no_link", ui.no_link()),
        ("cancelled_order", ui.cancelled(Some(ORDER))),
        ("cancelled_flow", ui.cancelled(None)),
        ("invoice_soon", ui.invoice_soon()),
        (
            "check_link_pending",
            ui.check_link_pending(Platform::CryptoBot),
        ),
        (
            "check_link_waiting",
            ui.check_link_waiting(Platform::XRocket, 14),
        ),
        (
            "check_link_missing",
            ui.check_link_missing(Platform::CryptoBot),
        ),
    ];
    let mut out: Vec<(String, Screen)> = screens
        .drain(..)
        .map(|(name, screen)| (name.to_owned(), screen))
        .collect();
    for (name, error) in all_errors() {
        out.push((format!("error_{name}"), ui.error(&error)));
    }
    for (name, reason) in all_refund_reasons() {
        out.push((
            format!("refund_started_{name}"),
            ui.refund_started(&order(cb), &usdt(dec!(750)), &reason),
        ));
    }
    out.extend(invest_gallery(ui));
    out
}

/// Инвестор: 1 250 в обороте, из них 300 — свежее пополнение под блокировкой.
pub fn investor() -> InvestHome {
    InvestHome {
        share_pct: dec!(50),
        lock_days: 14,
        min_deposit: dec!(10),
        auto_withdraw_max: dec!(200),
        balance: dec!(1250.75),
        profit: dec!(50.75),
        profit_24h: dec!(2.5),
        locked: dec!(300),
        withdrawable: dec!(950.75),
        moves: vec![
            InvestMoveRow {
                kind: InvestMoveKind::Withdrawal,
                status: InvestMoveStatus::Requested,
                platform: Platform::XRocket,
                amount: Some(dec!(500)),
                at: at(13, 10),
                check_url: None,
            },
            InvestMoveRow {
                kind: InvestMoveKind::Deposit,
                status: InvestMoveStatus::Done,
                platform: Platform::CryptoBot,
                amount: Some(dec!(300)),
                at: at(12, 0),
                check_url: None,
            },
            InvestMoveRow {
                kind: InvestMoveKind::Withdrawal,
                status: InvestMoveStatus::Done,
                platform: Platform::CryptoBot,
                amount: Some(dec!(40)),
                at: at(11, 30),
                check_url: Some(REFUND_URL.to_owned()),
            },
            InvestMoveRow {
                kind: InvestMoveKind::Deposit,
                status: InvestMoveStatus::Processing,
                platform: Platform::CryptoBot,
                amount: None,
                at: at(11, 0),
                check_url: None,
            },
        ],
    }
}

/// Новый инвестор: ничего нет, без блокировки, выводы только через владельца.
pub fn new_investor() -> InvestHome {
    InvestHome {
        share_pct: dec!(40),
        lock_days: 0,
        min_deposit: dec!(50),
        auto_withdraw_max: dec!(0),
        balance: dec!(0),
        profit: dec!(0),
        profit_24h: dec!(0),
        locked: dec!(0),
        withdrawable: dec!(0),
        moves: Vec::new(),
    }
}

pub fn invest_gallery(ui: &Ui) -> Vec<(String, Screen)> {
    let i = investor();
    let n = new_investor();
    let xr = Platform::XRocket;
    let screens = vec![
        ("invest_home", ui.invest_home(&i)),
        ("invest_home_new", ui.invest_home(&n)),
        ("invest_howto", ui.invest_howto(&i)),
        ("invest_howto_new", ui.invest_howto(&n)),
        ("invest_deposit", ui.invest_deposit_prompt(&i)),
        ("invest_withdraw", ui.invest_withdraw_menu(&i)),
        ("invest_withdraw_nothing", ui.invest_withdraw_menu(&n)),
        ("invest_withdraw_amount", ui.invest_withdraw_amount(&i, xr)),
        (
            "invest_notice_deposit_accepted",
            ui.invest_notice(&InvestNotice::DepositAccepted),
        ),
        (
            "invest_notice_deposit_done",
            ui.invest_notice(&InvestNotice::DepositDone {
                amount: dec!(300),
                balance: dec!(1250.75),
            }),
        ),
        (
            "invest_notice_deposit_rejected",
            ui.invest_notice(&InvestNotice::DepositRejected(UiError::AlreadyActivated)),
        ),
        (
            "invest_notice_deposit_manual",
            ui.invest_notice(&InvestNotice::DepositManual),
        ),
        (
            "invest_notice_withdraw_requested",
            ui.invest_notice(&InvestNotice::WithdrawRequested {
                amount: dec!(500),
                platform: xr,
            }),
        ),
        (
            "invest_notice_withdraw_processing",
            ui.invest_notice(&InvestNotice::WithdrawProcessing {
                amount: dec!(50),
                platform: xr,
            }),
        ),
        (
            "invest_notice_withdraw_done",
            ui.invest_notice(&InvestNotice::WithdrawDone {
                amount: dec!(50),
                platform: xr,
                check_url: CHECK_URL.to_owned(),
            }),
        ),
        (
            "invest_notice_withdraw_rejected",
            ui.invest_notice(&InvestNotice::WithdrawRejected {
                amount: dec!(500),
                reason: "рано, подождите неделю <b>&".to_owned(),
            }),
        ),
        (
            "invest_notice_withdraw_failed",
            ui.invest_notice(&InvestNotice::WithdrawFailed { amount: dec!(50) }),
        ),
        (
            "invest_notice_withdraw_manual",
            ui.invest_notice(&InvestNotice::WithdrawManual { amount: dec!(50) }),
        ),
        (
            "invest_problem_not_a_check",
            ui.invest_problem(&InvestProblem::NotACheck),
        ),
        (
            "invest_problem_bad_amount",
            ui.invest_problem(&InvestProblem::BadAmount),
        ),
        (
            "invest_problem_over",
            ui.invest_problem(&InvestProblem::OverWithdrawable { max: dec!(950.75) }),
        ),
        (
            "invest_problem_open",
            ui.invest_problem(&InvestProblem::OpenWithdrawal),
        ),
        (
            "invest_problem_wallet",
            ui.invest_problem(&InvestProblem::WalletUnavailable { platform: xr }),
        ),
        (
            "invest_problem_internal",
            ui.invest_problem(&InvestProblem::Internal),
        ),
    ];
    screens
        .into_iter()
        .map(|(name, screen)| (name.to_owned(), screen))
        .collect()
}
