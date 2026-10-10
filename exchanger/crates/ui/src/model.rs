//! Данные для экранов. Их собирает движок (engine) или обвязка бота; рендер их только читает.
//! Суммы — `Money`/`Decimal`, время — UTC.

use chrono::{DateTime, Utc};
use domain::{Decimal, Direction, Money, OrderState, Platform};

/// Состояние направления для клиента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectionStatus {
    Open,
    /// Временная пауза (резерв, сбой кошелька, ручная пауза).
    Paused,
    /// Направление выключено владельцем.
    Off,
}

/// Условия направления на сейчас (SPEC §2.3, §3.4). Суммы — в USDT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectionTerms {
    pub direction: Direction,
    /// Текущая комиссия, `2.5` = 2,5 %.
    pub fee_pct: Decimal,
    pub min_fee: Decimal,
    pub min: Decimal,
    /// Эффективный максимум с учётом резерва (`max_eff`).
    pub max: Decimal,
    pub status: DirectionStatus,
}

/// Условия всех направлений.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terms {
    /// Порядок строк в таблицах. Основной спрос — CryptoBot → xRocket — первым (DESIGN-v0.2 Р1).
    pub directions: Vec<DirectionTerms>,
    /// `@username` аккаунта сервиса, к которому можно привязать чек (`None` — не говорим).
    pub bind_username: Option<String>,
}

impl Terms {
    pub fn get(&self, direction: Direction) -> Option<&DirectionTerms> {
        self.directions.iter().find(|d| d.direction == direction)
    }

    pub(crate) fn open(&self) -> impl Iterator<Item = &DirectionTerms> {
        self.directions
            .iter()
            .filter(|d| d.status == DirectionStatus::Open)
    }
}

/// Заявка, к которой относится сообщение.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderRef {
    /// Публичный номер (`orders.public_id`, 10 символов).
    pub id: String,
    pub direction: Direction,
}

/// Расчёт обмена чека (SPEC §2.3): получено, комиссия, выплата.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breakdown {
    pub amount_in: Money,
    pub fee_pct: Decimal,
    pub fee: Money,
    pub payout: Money,
}

/// Счёт в фиате: сумма, код валюты и запас на курс.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FiatAmount {
    pub amount: Decimal,
    pub currency: String,
    pub buffer_pct: Decimal,
}

/// Котировка оплаты чужого счёта (SPEC §2.3, S5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceQuote {
    pub order: OrderRef,
    /// Сумма счёта в USDT (для фиатного — уже с запасом).
    pub invoice_amount: Money,
    pub fiat: Option<FiatAmount>,
    /// Описание счёта от продавца — чужой текст, экранируется и обрезается.
    pub description: Option<String>,
    pub fee_pct: Decimal,
    pub fee: Money,
    /// Сколько прислать клиенту (`X`).
    pub to_pay: Money,
    pub expires_at: DateTime<Utc>,
}

/// Переплата по счёту.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Excess {
    /// Вернём чеком.
    Refund(Money),
    /// Меньше минимального возврата — оставили в счёт комиссии.
    Kept { amount: Money, min_refund: Money },
}

/// Причина возврата (S9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefundReason {
    BelowMinimum { min: Money },
    AboveMaximum { max: Money },
    UnsupportedAsset,
    InvoicePaidElsewhere,
    InvoiceExpired,
    RateMoved,
    ClientRequest,
    Underpaid,
    Operator,
}

/// Почему не можем оплатить счёт (E17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvoiceProblem {
    Reusable,
    AlreadyPaid,
    Expired,
    NoAmount,
    UnsupportedAsset,
    ExpiresSoon,
    Unreadable,
}

/// Ошибки и отказы (SPEC §6.3, адаптировано к v0.2). Каждая говорит, где деньги клиента.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiError {
    /// E01: ссылка на бота кошелька, но не чек и не счёт; или вообще не ссылка кошелька.
    NotCheckOrInvoice,
    /// E02: похожий, но не официальный бот.
    ForeignBot {
        bot: String,
    },
    /// Ссылка тестовой сети.
    Testnet,
    /// E03.
    AlreadyActivated,
    /// E04.
    NotFound,
    /// E05: чек привязан к другому аккаунту. `bind_username` — к кому можно привязать.
    NotForUs {
        bind_username: Option<String>,
    },
    /// E06.
    PremiumOnly,
    /// E07.
    NeedsSubscription,
    Captcha,
    /// DESIGN-v0.2 Р3: чеки с паролем не принимаем.
    PasswordProtected,
    /// E08.
    MultiCheck,
    /// E09: после активации — возврат.
    BelowMinimum {
        amount_in: Money,
        min: Money,
        source: Platform,
    },
    /// E10: после активации — возврат.
    AboveMaximum {
        amount_in: Money,
        max: Money,
        source: Platform,
    },
    /// E11.
    DirectionPaused {
        direction: Direction,
        until: Option<DateTime<Utc>>,
    },
    /// E12: резерва нет на такую сумму.
    NoReserve {
        max: Money,
    },
    /// E13.
    Maintenance {
        until: Option<DateTime<Utc>>,
    },
    /// E14.
    RateLimited {
        retry_after_secs: u64,
    },
    /// E15.
    Banned {
        until: Option<DateTime<Utc>>,
        reason: String,
    },
    /// E16: этот чек уже в работе.
    Duplicate {
        order: String,
    },
    /// E17.
    InvoiceUnsuitable(InvoiceProblem),
    /// E18.
    PayoutDelayed {
        order: OrderRef,
        payout: Money,
    },
    /// E19.
    UnsupportedAsset {
        amount_in: Money,
        source: Platform,
    },
    /// E20.
    InvoicePaidElsewhere {
        to_pay: Money,
        source: Platform,
    },
    /// E21.
    RateMoved {
        to_pay: Money,
        source: Platform,
    },
    /// E23.
    QuoteExpired,
    /// E24.
    Internal,
}

/// Строка истории (S10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRow {
    pub order: String,
    pub created_at: DateTime<Utc>,
    pub direction: Direction,
    pub amount: Option<Money>,
    pub state: OrderState,
}

/// Карточка заявки (S11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderCard {
    pub order: OrderRef,
    pub state: OrderState,
    pub created_at: DateTime<Utc>,
    pub amount_in: Option<Money>,
    pub fee: Option<Money>,
    pub payout: Option<Money>,
    pub invoice_amount: Option<Money>,
    pub refunded: Option<Money>,
    /// Ссылка на чек выплаты или возврата, пока клиент его не забрал.
    pub check_url: Option<String>,
}
