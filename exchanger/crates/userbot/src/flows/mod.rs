//! Сценарии работы с @send (CryptoBot) и @xrocket поверх [`Transport`] — реализация
//! [`WalletBot`] для движка (DESIGN-v0.2 §4, LOVEC-PORTING §3, §8–§9).
//!
//! Устройство:
//! - [`AccountGate`] — общий на аккаунт Telegram: одно действие в полёте, темп (не чаще
//!   1 действия в 1,5 с и не больше 8 обращений к ботам в минуту), пауза FLOOD_WAIT, карантин
//!   чата после команды без ответа;
//! - `conv` — разговор одной операции: подписка до отправки, повтор отправки только с тем же
//!   `random_id`, поиск своей команды после `RANDOM_ID_DUPLICATE`, ожидание ответа (новые
//!   сообщения и правки с id больше id команды), догон по истории;
//! - сценарии: `activate`, `issue` (инлайн → меню, метка операции в «Избранном», поиск
//!   выданного чека), `invoice` (карточка, оплата), `balance`.
//!
//! Правила денег:
//! - `Err(BotError)` — операция не выполнялась, деньги не двигались: движок может закрыть
//!   операцию как неуспешную и создать новую;
//! - всё, что могло выполниться, возвращается исходом `Unknown`: после первого действия,
//!   которое могло создать чек или оплатить счёт, нет ни повторов с новым `random_id`, ни
//!   запасного пути;
//! - ответ бота засчитывается, только если его id больше id нашей команды: правка старого
//!   сообщения или поздний ответ на прошлую команду не выдаются за ответ на эту;
//! - каждое сообщение чатов кошелька уходит в [`MessageLog`] до разбора (CLAUDE.md, п. 8).

mod activate;
mod balance;
mod conv;
mod gate;
mod invoice;
mod issue;
pub mod rid;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use domain::{Asset, Money, Platform};
use serde::{Deserialize, Serialize};

pub use gate::AccountGate;
pub use issue::{is_marker, marker_text};

use crate::transport::{RawMessage, Transport};
use crate::wallet::{
    ActivationOutcome, BotError, InvoiceInfo, IssueOutcome, IssuedCheck, OpTag, PayOutcome,
    WalletBot,
};

/// Настройки сценариев. Значения по умолчанию — из разбора lovec (LOVEC-PORTING §3.6, §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowConfig {
    /// Сколько ждать ответа бота на одну команду или нажатие.
    pub reply_timeout: Duration,
    /// Минимум между действиями аккаунта.
    pub min_interval: Duration,
    /// Не больше стольких обращений к ботам за 60 с (команды, нажатия, инлайн-запросы).
    pub bot_actions_per_minute: u32,
    /// После команды без ответа чат столько ждёт поздний ответ, прежде чем принять следующую
    /// команду: поздний ответ не должен достаться чужой операции.
    pub late_reply_quarantine: Duration,
    /// xRocket иногда отвечает «не найден» на свежий чек: один повтор `/start` через это
    /// время (lovec `ledger.rs:19-20`). `None` — без повтора.
    pub xrocket_not_found_retry: Option<Duration>,
    /// Создавать чек через инлайн-режим; `false` — сразу через меню.
    pub inline_checks: bool,
    /// Сколько последних сообщений чата смотреть при поиске своей команды и выданного чека.
    pub scan_depth: u32,
}

impl Default for FlowConfig {
    fn default() -> Self {
        Self {
            reply_timeout: Duration::from_secs(20),
            min_interval: Duration::from_millis(1500),
            bot_actions_per_minute: 8,
            late_reply_quarantine: Duration::from_secs(30),
            xrocket_not_found_retry: Some(Duration::from_secs(2)),
            inline_checks: true,
            scan_depth: 100,
        }
    }
}

/// Журнал сообщений ботов кошельков (`wallet_messages`). Вызывается до разбора. Одно и то же
/// сообщение может прийти второй раз с ключом операции — когда сценарий признал его своим.
#[async_trait]
pub trait MessageLog: Send + Sync {
    async fn record(&self, account: &str, msg: &RawMessage, op: Option<&str>);
}

/// Журнал, который ничего не хранит.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoLog;

#[async_trait]
impl MessageLog for NoLog {
    async fn record(&self, _account: &str, _msg: &RawMessage, _op: Option<&str>) {}
}

/// Запись журнала в памяти (тесты, режим записи).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedMessage {
    pub account: String,
    pub message: RawMessage,
    pub op: Option<String>,
}

/// Журнал в памяти.
#[derive(Debug, Default)]
pub struct MemoryLog {
    entries: Mutex<Vec<LoggedMessage>>,
}

impl MemoryLog {
    pub fn entries(&self) -> Vec<LoggedMessage> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl MessageLog for MemoryLog {
    async fn record(&self, account: &str, msg: &RawMessage, op: Option<&str>) {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(LoggedMessage {
                account: account.to_owned(),
                message: msg.clone(),
                op: op.map(str::to_owned),
            });
    }
}

/// Итог шага мини-приложения оплаты (CryptoBot: PIN и подтверждение).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WebAppOutcome {
    Paid,
    AlreadyPaid,
    Expired,
    InsufficientFunds,
    /// Точно не оплачено (неверный PIN, блокировка, отказ) — нужен человек.
    NotPaid(String),
    /// Запрос оплаты ушёл, исход неизвестен.
    Unknown(String),
}

/// Проход мини-приложения оплаты по URL сессии из `Transport::open_webapp`.
///
/// Протокол мини-приложения CryptoBot неизвестен до записи HAR владельцем (HANDOVER §7,
/// п. 4). Пока реализации нет, `pay_invoice` у CryptoBot заканчивается `NeedsHuman`.
#[async_trait]
pub trait WebAppPayer: Send + Sync {
    async fn pay(&self, platform: Platform, session_url: &str) -> WebAppOutcome;
}

/// Кошелёк одной платформы на аккаунте юзербота.
pub struct UserbotWallet<T: Transport> {
    platform: Platform,
    transport: Arc<T>,
    gate: Arc<AccountGate>,
    log: Arc<dyn MessageLog>,
    payer: Option<Arc<dyn WebAppPayer>>,
    cfg: FlowConfig,
}

impl<T: Transport> std::fmt::Debug for UserbotWallet<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserbotWallet")
            .field("platform", &self.platform)
            .field("account", &self.gate.label())
            .finish_non_exhaustive()
    }
}

impl<T: Transport> UserbotWallet<T> {
    /// `gate` — общий для всех кошельков одного аккаунта Telegram.
    pub fn new(platform: Platform, transport: Arc<T>, gate: Arc<AccountGate>) -> Self {
        Self {
            platform,
            transport,
            gate,
            log: Arc::new(NoLog),
            payer: None,
            cfg: FlowConfig::default(),
        }
    }

    pub fn with_log(mut self, log: Arc<dyn MessageLog>) -> Self {
        self.log = log;
        self
    }

    pub fn with_payer(mut self, payer: Arc<dyn WebAppPayer>) -> Self {
        self.payer = Some(payer);
        self
    }

    pub fn with_config(mut self, cfg: FlowConfig) -> Self {
        self.cfg = cfg;
        self
    }

    pub fn config(&self) -> &FlowConfig {
        &self.cfg
    }

    pub fn gate(&self) -> &Arc<AccountGate> {
        &self.gate
    }
}

#[async_trait]
impl<T: Transport> WalletBot for UserbotWallet<T> {
    fn platform(&self) -> Platform {
        self.platform
    }

    async fn activate_check(
        &self,
        param: &str,
        tag: &OpTag,
    ) -> Result<ActivationOutcome, BotError> {
        activate::run(self, param, tag).await
    }

    async fn issue_check(&self, amount: &Money, tag: &OpTag) -> Result<IssueOutcome, BotError> {
        issue::run(self, amount, tag).await
    }

    async fn find_issued_check(
        &self,
        amount: &Money,
        tag: &OpTag,
    ) -> Result<Option<IssuedCheck>, BotError> {
        issue::find(self, amount, tag).await
    }

    async fn inspect_invoice(&self, param: &str) -> Result<InvoiceInfo, BotError> {
        invoice::inspect(self, param).await
    }

    async fn pay_invoice(
        &self,
        param: &str,
        asset: Asset,
        expected: &Money,
        tag: &OpTag,
    ) -> Result<PayOutcome, BotError> {
        invoice::pay(self, param, asset, expected, tag).await
    }

    async fn balances(&self) -> Result<Vec<Money>, BotError> {
        balance::run(self).await
    }
}

/// Сумма так, как её вводит человек: `97.5`, `10`, `0.25`.
pub(crate) fn typed_amount(m: &Money) -> String {
    m.amount().normalize().to_string()
}
