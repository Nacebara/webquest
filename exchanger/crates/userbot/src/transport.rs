//! Транспорт юзербота — всё, что сценарии делают в Telegram от имени аккаунта.
//!
//! Реализации: `mtproto::GrammersTransport` (настоящий аккаунт через grammers 0.10) и
//! `userbot::sim` (симулятор @send и @xrocket для тестов). Сценарии (`flows`) знают только
//! этот трейт, поэтому одинаково работают в бою и в тестах.
//!
//! Идемпотентность (факт из lovec, `tests/send_outcomes.rs`): повтор `messages.sendMessage`
//! или `messages.sendInlineBotResult` с ТЕМ ЖЕ `random_id` безопасен — Telegram не создаёт
//! второе сообщение (`RANDOM_ID_DUPLICATE` или прежний результат). Поэтому каждая денежная
//! операция получает свой `random_id` заранее и хранит его в БД.

use std::time::Duration;

use async_trait::async_trait;
use domain::Platform;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Чат, в котором действует юзербот.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Chat {
    /// Личка с ботом кошелька (@send или @xrocket) по закреплённому peer id.
    WalletBot(Platform),
    /// «Избранное» аккаунта: сюда юзербот отправляет инлайн-чеки, чтобы взять из них ссылку.
    SavedMessages,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ButtonKind {
    /// Callback-кнопка: `messages.getBotCallbackAnswer` с этими данными.
    Callback(Vec<u8>),
    Url(String),
    /// Кнопка мини-приложения (WebView / SimpleWebView): URL для `requestWebView`.
    WebApp(String),
    /// `switch_inline_query`.
    SwitchInline(String),
    /// Копирование текста (copy_text).
    CopyText(String),
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawButton {
    pub text: String,
    pub kind: ButtonKind,
}

/// Сообщение в чате юзербота (новое или отредактированное).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawMessage {
    pub chat: Chat,
    pub id: i32,
    /// Отправлено нами.
    pub out: bool,
    /// Unix-время отправки.
    pub date: i64,
    /// Unix-время правки; `None` — исходная версия.
    pub edit_date: Option<i64>,
    pub text: String,
    /// URL из сущностей `text_link` / `url`.
    pub entity_urls: Vec<String>,
    /// Инлайн-клавиатура по рядам.
    pub buttons: Vec<Vec<RawButton>>,
    /// Сообщение отправлено через инлайн-режим этого бота (`via_bot`).
    pub via_bot: Option<Platform>,
}

impl RawMessage {
    pub fn button_urls(&self) -> Vec<String> {
        self.buttons
            .iter()
            .flatten()
            .filter_map(|b| match &b.kind {
                ButtonKind::Url(u) | ButtonKind::WebApp(u) => Some(u.clone()),
                _ => None,
            })
            .collect()
    }

    /// Первая кнопка, текст которой содержит подстроку (без учёта регистра).
    pub fn find_button(&self, needle: &str) -> Option<&RawButton> {
        let needle = needle.to_lowercase();
        self.buttons
            .iter()
            .flatten()
            .find(|b| b.text.to_lowercase().contains(&needle))
    }
}

/// Один вариант инлайн-ответа бота (`@send 10usdt` → «Отправить чек на 10 USDT»).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineResult {
    pub id: String,
    pub title: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineResults {
    pub query_id: i64,
    pub results: Vec<InlineResult>,
}

/// Ответ на нажатие callback-кнопки (всплывающее уведомление или переход по URL).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CallbackAnswer {
    pub message: Option<String>,
    pub alert: bool,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("FLOOD_WAIT {0:?}")]
    FloodWait(Duration),
    /// Исход неизвестен: запрос мог выполниться. Повторять только с тем же `random_id`.
    #[error("timeout, outcome unknown")]
    Timeout,
    #[error("disconnected, outcome unknown")]
    Disconnected,
    /// Повтор с тем же `random_id`: сообщение уже было отправлено раньше.
    #[error("RANDOM_ID_DUPLICATE")]
    RandomIdDuplicate,
    #[error("account is not authorized")]
    NotAuthorized,
    #[error("RPC error {code} {name}")]
    Rpc { code: i32, name: String },
    #[error("{0}")]
    Other(String),
}

impl TransportError {
    /// Мог ли запрос выполниться на сервере (исход неизвестен).
    pub fn outcome_unknown(&self) -> bool {
        matches!(self, TransportError::Timeout | TransportError::Disconnected)
    }
}

#[async_trait]
pub trait Transport: Send + Sync + 'static {
    /// Отправить текст. `random_id` обязателен и уникален для операции: повтор с ним безопасен.
    async fn send_text(
        &self,
        chat: Chat,
        text: &str,
        random_id: i64,
    ) -> Result<RawMessage, TransportError>;

    /// Нажать callback-кнопку сообщения.
    async fn press(
        &self,
        chat: Chat,
        msg_id: i32,
        data: &[u8],
    ) -> Result<CallbackAnswer, TransportError>;

    /// Инлайн-запрос к боту кошелька (`10usdt` для @send, `10` для @xrocket).
    async fn inline_query(
        &self,
        bot: Platform,
        query: &str,
    ) -> Result<InlineResults, TransportError>;

    /// Отправить выбранный инлайн-результат в чат. `random_id` — как в `send_text`.
    async fn send_inline(
        &self,
        to: Chat,
        bot: Platform,
        results: &InlineResults,
        result_id: &str,
        random_id: i64,
    ) -> Result<RawMessage, TransportError>;

    /// Открыть мини-приложение бота (`messages.requestWebView`): URL с `tgWebAppData`.
    async fn open_webapp(&self, bot: Platform, button_url: &str) -> Result<String, TransportError>;

    /// Сообщения чата с id > `after_id`, по возрастанию id (догон после рестарта).
    async fn history(
        &self,
        chat: Chat,
        after_id: i32,
        limit: u32,
    ) -> Result<Vec<RawMessage>, TransportError>;

    /// Последние `limit` сообщений чата по возрастанию id: поиск своей команды после
    /// `RandomIdDuplicate` и выданного чека после сбоя, когда id-якоря нет.
    async fn recent(&self, chat: Chat, limit: u32) -> Result<Vec<RawMessage>, TransportError>;

    /// Поток новых и отредактированных сообщений в чатах с ботами кошельков и в «Избранном».
    fn subscribe(&self) -> broadcast::Receiver<RawMessage>;
}
