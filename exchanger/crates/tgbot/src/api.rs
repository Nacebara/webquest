//! Вызовы Bot API с повтором по `retry_after` и отправка экранов `ui`.
//!
//! Правила повтора:
//! - 429 (`retry_after`) — ждём сколько сказал Telegram и повторяем: запрос не выполнялся;
//! - соединение не установилось — повторяем: запрос до Telegram не дошёл;
//! - таймаут или разрыв после отправки — `sendMessage` **не** повторяем (будет дубль
//!   сообщения), правку повторяем (она идемпотентна).

use std::future::Future;
use std::time::Duration;

use botapi::{
    AnswerCallbackQuery, BotApi, EditMessageText, Error, Message, SendMessage, SendRichMessage,
};
use ui::{Keyboard, Screen};

/// Сколько раз пробовать один вызов.
const MAX_ATTEMPTS: u32 = 4;
/// Больше этого `retry_after` не ждём внутри одного обновления.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);
const BACKOFF: Duration = Duration::from_millis(500);

/// Итог правки сообщения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edited {
    Done,
    /// Сообщение уже такое — тоже успех.
    NotModified,
    /// Сообщения нет или его нельзя править (клиент удалил, слишком старое).
    Gone,
}

#[derive(Debug, Clone)]
pub(crate) struct Api {
    bot: BotApi,
}

impl Api {
    pub(crate) fn new(bot: BotApi) -> Self {
        Self { bot }
    }

    pub(crate) fn bot(&self) -> &BotApi {
        &self.bot
    }

    /// Вызов с повтором. `idempotent` — повтор после неясного исхода не создаст дубль.
    async fn retry<T, F, Fut>(&self, method: &str, idempotent: bool, call: F) -> Result<T, Error>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T, Error>>,
    {
        let mut attempt = 1;
        loop {
            let err = match call().await {
                Ok(value) => return Ok(value),
                Err(err) => err,
            };
            if attempt >= MAX_ATTEMPTS {
                return Err(err);
            }
            let wait = if let Some(wait) = err.retry_after() {
                if wait > MAX_RETRY_AFTER {
                    return Err(err);
                }
                wait
            } else if err.not_delivered() || (idempotent && err.outcome_unknown()) {
                BACKOFF * attempt
            } else {
                return Err(err);
            };
            tracing::warn!(method, attempt, wait_ms = wait.as_millis(), error = %err, "Bot API call failed, retrying");
            tokio::time::sleep(wait).await;
            attempt += 1;
        }
    }

    /// Экран обычным HTML (денежный поток и все экраны без rich).
    pub(crate) async fn send_html(&self, chat: i64, screen: &Screen) -> Result<Message, Error> {
        self.send_html_with(chat, &screen.html, &screen.keyboard)
            .await
    }

    async fn send_html_with(
        &self,
        chat: i64,
        html: &str,
        keyboard: &Keyboard,
    ) -> Result<Message, Error> {
        let mut request = SendMessage::html(chat, html);
        request.reply_markup = keyboard.reply_markup();
        self.retry("sendMessage", false, || self.bot.send_message(&request))
            .await
    }

    /// Справочный экран rich-сообщением; Telegram отказал по существу — HTML-фолбэк.
    /// Возвращает сообщение и `true`, если ушло rich.
    pub(crate) async fn send_rich(
        &self,
        chat: i64,
        rich_html: &str,
        rich_keyboard: &Keyboard,
        screen: &Screen,
    ) -> Result<(Message, bool), Error> {
        let mut request = SendRichMessage::new(chat, rich_html);
        request.reply_markup = rich_keyboard.reply_markup();
        match self
            .retry("sendRichMessage", false, || {
                self.bot.send_rich_message(&request)
            })
            .await
        {
            Ok(message) => Ok((message, true)),
            Err(err) if err.is_rejection() => {
                tracing::info!(error = %err, "rich message rejected, sending the HTML fallback");
                let message = self.send_html(chat, screen).await?;
                Ok((message, false))
            }
            Err(err) => Err(err),
        }
    }

    /// Править сообщение обычным HTML. У экрана может быть только инлайн-клавиатура.
    pub(crate) async fn edit_html(
        &self,
        chat: i64,
        message_id: i32,
        screen: &Screen,
    ) -> Result<Edited, Error> {
        let mut request = EditMessageText::html(chat, message_id, screen.html.as_str());
        request.reply_markup = screen.keyboard.inline().cloned();
        match self
            .retry("editMessageText", true, || {
                self.bot.edit_message_text(&request)
            })
            .await
        {
            Ok(_) => Ok(Edited::Done),
            Err(err) if err.is_not_modified() => Ok(Edited::NotModified),
            Err(err) if err.is_message_gone() => Ok(Edited::Gone),
            Err(err) => Err(err),
        }
    }

    /// Ответ на нажатие кнопки (убрать «часики»). Ошибка — только в лог: кнопка старая или
    /// запрос уже отвечен.
    pub(crate) async fn answer(&self, callback_id: &str) {
        let request = AnswerCallbackQuery::ack(callback_id);
        if let Err(err) = self
            .retry("answerCallbackQuery", true, || {
                self.bot.answer_callback_query(&request)
            })
            .await
        {
            tracing::debug!(error = %err, "answerCallbackQuery failed");
        }
    }
}
