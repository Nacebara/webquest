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
    Rejected(RejectReason),
    /// Ответа нет или он незнакомый — сверка по балансу и истории.
    Unknown { raw: Option<RawMessage> },
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
    /// `@send 10usdt` / `@xrocket 10` → результат в «Избранное».
    Inline,
    /// `/checks` → «Создать чек» / `/cheques` → «Персональный» → «Создать чек».
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

/// Чужой счёт, открытый без оплаты.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceInfo {
    pub param: String,
    pub card: parsers::InvoiceCard,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PayOutcome {
    Paid,
    AlreadyPaid,
    Expired,
    InsufficientFunds,
    /// Нужен человек: PIN в мини-приложении без известного протокола, незнакомый экран.
    NeedsHuman { reason: String },
    /// Нажатие ушло, исход неизвестен. Нажимать снова нельзя.
    Unknown { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BotError {
    /// Аккаунт на паузе FLOOD_WAIT; операция не выполнялась.
    #[error("flood wait {0:?}")]
    FloodWait(std::time::Duration),
    /// Операция не начата: транспорт недоступен.
    #[error("transport unavailable: {0}")]
    Unavailable(String),
    /// Кошелёк сообщил об ограничении аккаунта — направление на паузу (SPEC §4, П15).
    #[error("wallet account restricted")]
    AccountRestricted,
    #[error(transparent)]
    Transport(#[from] TransportError),
}

#[async_trait]
pub trait WalletBot: Send + Sync {
    fn platform(&self) -> Platform;

    /// `/start <param>` боту кошелька и разбор ответа.
    async fn activate_check(&self, param: &str, tag: &OpTag)
    -> Result<ActivationOutcome, BotError>;

    /// Создать чек на `amount`: инлайн-режим, при неудаче — меню бота. Одна попытка.
    async fn issue_check(&self, amount: &Money, tag: &OpTag) -> Result<IssueOutcome, BotError>;

    /// Сверка после `IssueOutcome::Unknown`: есть ли в «Избранном» / списке чеков чек этой
    /// операции (по `random_id`, сумме и времени).
    async fn find_issued_check(
        &self,
        amount: &Money,
        tag: &OpTag,
    ) -> Result<Option<IssuedCheck>, BotError>;

    /// Открыть чужой счёт без оплаты.
    async fn inspect_invoice(&self, param: &str) -> Result<InvoiceInfo, BotError>;

    /// Оплатить чужой счёт: валюта кнопкой → «Оплатить» → (CryptoBot) мини-приложение и PIN.
    async fn pay_invoice(
        &self,
        param: &str,
        asset: Asset,
        expected: &Money,
        tag: &OpTag,
    ) -> Result<PayOutcome, BotError>;

    /// Балансы кошелька по активам (экран «Кошелёк»).
    async fn balances(&self) -> Result<Vec<Money>, BotError>;
}
