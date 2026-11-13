//! Операции кошелька, которые нужны движку (engine). Реализация — `UserbotWallet<T: Transport>`
//! в `flows`; в тестах engine работает через неё же с транспортом-симулятором.
//!
//! Правила денег (CLAUDE.md, SPEC §10.5):
//! - `activate_check` можно повторять (второй раз чек не зачислят);
//! - `issue_check` и `pay_invoice` — ОДНА попытка: при неясном исходе возвращают `Unknown`,
//!   а движок отправляет заявку на сверку (`find_issued_check`) или человеку;
//! - повтор отправки внутри одной попытки — только с тем же `random_id` из `OpTag`.

use async_trait::async_trait;
use domain::{Asset, Money, Platform};
use serde::{Deserialize, Serialize};

use crate::transport::{RawMessage, TransportError};

/// Метка операции: ключ из `operations.idempotency_key` и заранее сохранённый `random_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpTag {
    pub key: String,
    pub random_id: i64,
}

/// Почему чек клиента не принят. Деньги клиента при этом не у нас.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectReason {
    AlreadyActivated,
    NotFound,
    NotForUs,
    NeedsSubscription,
    Captcha,
    PasswordProtected,
    PremiumOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivationOutcome {
    Received(Money),
    /// Чек в монете, которую сервис не принимает (XROCK, GRAM…): деньги в кошельке, вернуть
    /// их может только человек.
    ReceivedOther {
        amount: domain::Decimal,
        code: String,
    },
    Rejected(RejectReason),
    /// Ответа нет или он незнакомый — сверка по балансу и истории.
    Unknown {
        raw: Option<RawMessage>,
    },
}

/// Чек, созданный юзерботом для клиента.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuedCheck {
    pub url: String,
    pub param: String,
    pub amount: Money,
}

/// Как был создан чек — для журнала и статистики.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IssueMethod {
    /// `/checks` → «Создать чек» / `/cheques` → «Персональный» → «Создать чек»
    /// (единственный способ: DESIGN-v0.2 Р5).
    Menu,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IssueOutcome {
    Issued {
        check: IssuedCheck,
        method: IssueMethod,
    },
    /// Бот сообщил, что денег не хватает. Чек точно не создан.
    InsufficientFunds,
    /// Исход неизвестен: чек мог быть создан. Повторять нельзя — сначала `find_issued_check`.
    Unknown { detail: String },
}

/// Сумма чека CryptoBot осталась без ответа (или ответов на неё два): чек мог создать сама сумма,
/// а его поздний экран — новое сообщение. Чат кошелька занят, пока экран не придёт или заявку не
/// разберёт человек (аудит 11.11.2026, шаг 1.2 — как активация без ответа в Ю-2).
pub const CHAT_HELD_NO_REPLY: &str = "no reply to the check amount: the wallet chat is held";
/// Два ответа на одну сумму чека CryptoBot: в окно попал поздний экран прошлой операции.
pub const CHAT_HELD_TWO_REPLIES: &str = "two replies to one check amount: the wallet chat is held";

impl IssueOutcome {
    /// Исход неясен, и чат кошелька занят этой суммой ([`CHAT_HELD_NO_REPLY`],
    /// [`CHAT_HELD_TWO_REPLIES`]): приём и выплаты через кошелёк — на паузу.
    pub fn holds_chat(&self) -> bool {
        matches!(self, IssueOutcome::Unknown { detail }
            if detail == CHAT_HELD_NO_REPLY || detail == CHAT_HELD_TWO_REPLIES)
    }
}

/// Чужой счёт, открытый без оплаты.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceInfo {
    pub param: String,
    pub card: parsers::InvoiceCard,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PayOutcome {
    /// Оплачено; `amount` — сколько списано (у счёта в фиате может отличаться от ожидаемого
    /// в пределах [`PayDue`]).
    Paid {
        amount: Money,
    },
    /// Сумма к оплате вне допустимой ([`PayDue`]): у счёта в фиате сдвинулся курс. Ничего не
    /// нажато, деньги не двигались.
    AmountChanged {
        due: Money,
    },
    AlreadyPaid,
    Expired,
    InsufficientFunds,
    /// Нужен человек: бот просит код-пароль, кнопка ведёт в мини-приложение, незнакомый экран.
    /// Деньги не двигались.
    NeedsHuman {
        reason: String,
    },
    /// Нажатие ушло, исход неизвестен. Нажимать снова нельзя.
    Unknown {
        detail: String,
    },
}

/// Сколько можно заплатить по счёту. Счёт в USDT — ровно его сумма; счёт в фиате (USD) — сумма
/// в USDT с кнопки монеты на момент котировки, а при оплате — то, что покажет бот, если это не
/// больше `max` (запас на курс, его покрывает сервис) и не меньше `min`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayDue {
    pub expected: Money,
    pub min: Money,
    pub max: Money,
}

impl PayDue {
    /// Ровно эта сумма.
    pub fn exact(m: Money) -> Self {
        Self {
            expected: m,
            min: m,
            max: m,
        }
    }

    /// Подходит ли сумма на экране бота.
    pub fn fits(&self, amount: domain::Decimal, asset: Asset) -> bool {
        asset == self.expected.asset()
            && amount > domain::Decimal::ZERO
            && amount >= self.min.amount()
            && amount <= self.max.amount()
    }
}

/// Операция не выполнялась: деньги не двигались, движок может закрыть операцию как
/// неуспешную и создать новую. Всё, что могло выполниться, сценарии возвращают не ошибкой,
/// а исходом `Unknown` (`ActivationOutcome`, `IssueOutcome`, `PayOutcome`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BotError {
    /// Аккаунт на паузе FLOOD_WAIT; операция не выполнялась.
    #[error("flood wait {0:?}")]
    FloodWait(std::time::Duration),
    /// Операция не начата: транспорт недоступен.
    #[error("transport unavailable: {0}")]
    Unavailable(String),
    /// Операция не начата: чат кошелька занят активацией чека, оставшейся без ответа, и ждёт
    /// её ответа или решения человека (аудит 11.11.2026, Ю-2).
    #[error("the wallet chat is blocked by a check activation without a reply")]
    ChatBlocked,
    /// Кошелёк сообщил об ограничении аккаунта — направление на паузу (SPEC §4, П15).
    #[error("wallet account restricted")]
    AccountRestricted,
    /// Сессия аккаунта недействительна (разлогинен, отозвана) — нужен новый `exch login`.
    #[error("wallet account is not authorized")]
    NotAuthorized,
    #[error(transparent)]
    Transport(#[from] TransportError),
}

#[async_trait]
pub trait WalletBot: Send + Sync {
    fn platform(&self) -> Platform;

    /// `/start <param>` боту кошелька и разбор ответа.
    async fn activate_check(&self, param: &str, tag: &OpTag)
    -> Result<ActivationOutcome, BotError>;

    /// Создать чек на `amount` через меню бота. Одна попытка.
    async fn issue_check(&self, amount: &Money, tag: &OpTag) -> Result<IssueOutcome, BotError>;

    /// Сверка после `IssueOutcome::Unknown` или рестарта: чек этой операции между её меткой
    /// в «Избранном» и следующей меткой (экран меню в чате бота). `None` — чека нет или
    /// связать его с операцией нельзя: решает человек.
    async fn find_issued_check(
        &self,
        amount: &Money,
        tag: &OpTag,
    ) -> Result<Option<IssuedCheck>, BotError>;

    /// Открыть чужой счёт без оплаты.
    async fn inspect_invoice(&self, param: &str) -> Result<InvoiceInfo, BotError>;

    /// Оплатить чужой счёт кнопками в чате бота, без код-пароля. Нажатие, которое платит, —
    /// одно (CryptoBot: «💳 Оплатить» и, для нового получателя, «💸 Отправить платеж
    /// получателю»; xRocket: «Оплатить»).
    async fn pay_invoice(
        &self,
        param: &str,
        asset: Asset,
        due: &PayDue,
        tag: &OpTag,
    ) -> Result<PayOutcome, BotError>;

    /// Балансы кошелька по активам (экран «Кошелёк»).
    async fn balances(&self) -> Result<Vec<Money>, BotError>;

    /// Человек разобрал заявку, чья активация осталась без ответа: чат кошелька можно
    /// использовать снова (см. `flows::conv`, «чистый чат»). По умолчанию — ничего.
    async fn acknowledge_unanswered(&self) {}
}
