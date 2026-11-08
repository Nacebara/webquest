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
    /// Текущая комиссия, `2.5` = 2,5 %. Со ступенями — ставка самой дешёвой ступени.
    pub fee_pct: Decimal,
    pub min_fee: Decimal,
    pub min: Decimal,
    /// Эффективный максимум с учётом резерва (`max_eff`).
    pub max: Decimal,
    pub status: DirectionStatus,
    /// Ступени комиссии по сумме (с рычагом резерва); пусто — одна ставка на любую сумму.
    pub tiers: Vec<TierTerms>,
    /// Выгодный курс встречного потока сейчас (DESIGN-v0.2 Р15).
    pub promo: Option<PromoTerms>,
    /// У клиента личные условия на этом направлении.
    pub personal: bool,
    /// Почему на паузе (только при [`DirectionStatus::Paused`]).
    pub pause: Option<PauseReason>,
    /// Лимит первых обменов нового клиента, если он ниже обычного («Первые обмены — до 100»).
    pub new_user_max: Option<Decimal>,
}

/// Почему направление на паузе — клиенту, без внутренних подробностей.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseReason {
    /// На кошельке выплаты кончился резерв.
    NoReserve { wallet: Platform },
    /// Бот на обслуживании.
    Maintenance,
    /// Сбой кошелька или юзербота: автопауза, кошелёк не подключён.
    Technical,
}

/// Ступень: «от суммы `from` — `fee_pct`».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierTerms {
    pub from: Decimal,
    pub fee_pct: Decimal,
}

/// Выгодный курс: `fee_pct` на сумму до `up_to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromoTerms {
    pub fee_pct: Decimal,
    pub up_to: Decimal,
}

/// Условия всех направлений.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terms {
    /// Порядок строк в таблицах. Основной спрос — CryptoBot → xRocket — первым (DESIGN-v0.2 Р1).
    pub directions: Vec<DirectionTerms>,
    /// `@username` аккаунта сервиса, к которому можно привязать чек (`None` — не говорим).
    pub bind_username: Option<String>,
    /// Когда условия посчитаны: подвал «Обновлено 14:05» в «Курс и лимиты».
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
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

/// Почему не берём подходящий счёт прямо сейчас. Денег клиента у нас нет.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvoiceRefusal {
    Paused,
    Maintenance,
    DailyLimit,
    NoReserve { max: Money },
    BelowMinimum { min: Money },
    AboveMaximum { max: Money },
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
    /// В фиате: сумма в USDT плавает, такие пока не оплачиваем.
    Fiat,
    /// Кошелёк не показал счёт.
    Unavailable,
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
    /// E08: мульти-чек xRocket — активируется только в приложении xRocket.
    MultiCheck,
    /// Этот чек мы уже открывали (в том числе свою долю мульти-чека CryptoBot).
    CheckUsed,
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
    /// Суточный лимит обменов клиента исчерпан (`settings.daily_limit_usd`).
    DailyLimit,
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
    /// Этот счёт уже в работе.
    InvoiceInWork {
        order: String,
    },
    /// У клиента уже есть счёт, который ждёт чек.
    InvoicePending {
        order: String,
    },
    /// По счёту ещё проверяется предыдущий чек; этот не открывали.
    InvoiceCheckBusy,
    /// Счёт подходит, но взять его сейчас нельзя (лимиты, резерв, пауза).
    InvoiceRefused(InvoiceRefusal),
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
    /// «Итог»: выплата обмена или сумма оплаченного счёта.
    pub payout: Option<Money>,
    pub state: OrderState,
}

/// «Всего 23 обмена · 2 310 USDT».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistorySummary {
    pub completed: i64,
    pub volume: Decimal,
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
    /// Комиссия заявки, % — «2.50 · 2.5 %».
    pub fee_pct: Option<Decimal>,
    /// Когда закончилась — «· 9 секунд» в строке статуса.
    pub finished_at: Option<DateTime<Utc>>,
}

/// Вид движения инвестора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvestMoveKind {
    Deposit,
    Withdrawal,
}

/// Статус движения инвестора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvestMoveStatus {
    /// Вывод ждёт решения владельца.
    Requested,
    /// Чек активируется или создаётся.
    Processing,
    Done,
    Rejected,
    Failed,
    /// Неясный исход — разбирает владелец.
    Manual,
}

/// Строка «последних движений».
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvestMoveRow {
    pub kind: InvestMoveKind,
    pub status: InvestMoveStatus,
    pub platform: Platform,
    /// У пополнения — после зачисления.
    pub amount: Option<Decimal>,
    pub at: DateTime<Utc>,
    /// Чек вывода, если создан: ссылка в строке — на случай, если уведомление потерялось.
    pub check_url: Option<String>,
}

/// Экран инвестора (DESIGN-v0.2 Р18). Суммы — в USDT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvestHome {
    /// Доля прибыли на его капитал, `50` = 50 %.
    pub share_pct: Decimal,
    /// Пополнение выводится не раньше, чем через столько дней.
    pub lock_days: i32,
    pub min_deposit: Decimal,
    /// Вывод до этой суммы — сам, без владельца; 0 — только через владельца.
    pub auto_withdraw_max: Decimal,
    /// Сколько его денег в обороте.
    pub balance: Decimal,
    pub profit: Decimal,
    pub profit_24h: Decimal,
    /// Пополнения моложе срока блокировки.
    pub locked: Decimal,
    pub withdrawable: Decimal,
    pub moves: Vec<InvestMoveRow>,
}

/// Событие по движению инвестора — сообщение ему.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvestNotice {
    /// Чек пополнения принят, активирую.
    DepositAccepted,
    DepositDone {
        amount: Decimal,
        balance: Decimal,
    },
    /// Чек не открыт или не принят — экран ошибки чека.
    DepositRejected(UiError),
    DepositManual,
    /// Заявка ушла владельцу.
    WithdrawRequested {
        amount: Decimal,
        platform: Platform,
    },
    /// Чек создаётся (сразу или после одобрения).
    WithdrawProcessing {
        amount: Decimal,
        platform: Platform,
    },
    WithdrawDone {
        amount: Decimal,
        platform: Platform,
        check_url: String,
    },
    WithdrawRejected {
        amount: Decimal,
        reason: String,
    },
    WithdrawFailed {
        amount: Decimal,
    },
    WithdrawManual {
        amount: Decimal,
    },
}

/// Почему действие инвестора не принято. Деньги не тронуты.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvestProblem {
    /// Прислали не чек (счёт, мусор).
    NotACheck,
    /// Сумма не разобрана или ≤ 0.
    BadAmount,
    /// Больше, чем можно вывести сейчас.
    OverWithdrawable { max: Decimal },
    /// Уже есть заявка на вывод в работе.
    OpenWithdrawal,
    /// Кошелёк этой платформы сейчас недоступен.
    WalletUnavailable { platform: Platform },
    /// Сбой у сервиса.
    Internal,
}

/// Страница «Истории»: номер (с нуля), сколько всего и имя бота для ссылок «Когда» → карточка.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryPage {
    pub page: u32,
    pub pages: u32,
    /// Имя бота: «Когда» в строке — ссылка `t.me/<бот>?start=ord_<номер>`. Нет — кнопки.
    pub bot: Option<String>,
}
