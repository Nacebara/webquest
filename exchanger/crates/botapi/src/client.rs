//! HTTP-клиент Bot API по образцу lovec `bot_call` (notifier.rs:685-724): POST JSON на
//! `{base}/bot{token}/{method}`, ответ `{ok, result}` или `{ok:false, description, parameters}`.
//!
//! Отличия от lovec:
//! - на 429 клиент не ждёт сам, а возвращает `Error::Api` с `retry_after`: лимиты на чат и
//!   повторы — забота очереди отправки (SPEC §8.1), а не транспорта;
//! - TLS — rustls на ring с корнями webpki-roots (как у sqlx), без aws-lc;
//! - прокси из окружения не подхватывается: только явно через [`BotApiBuilder::proxy`].
//!
//! Токен живёт в `SecretString` и попадает только в URL запроса. В ошибки и логи он не
//! попадает: `without_url()` у ошибок reqwest плюс вычистка строк (см. `error.rs`).

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{CONTENT_TYPE, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Error, TransportKind, decode_response, scrub};
use crate::keyboard::InlineKeyboardMarkup;
use crate::methods::{
    AnswerCallbackQuery, DeleteMessage, EditMessageText, GetMe, GetUpdates, Method, SendMessage,
    SendRichMessage, SetMyCommands, parse_updates,
};
use crate::types::{BotCommand, Message, Update, User};

pub const DEFAULT_BASE_URL: &str = "https://api.telegram.org";
/// Таймаут обычного запроса (как у lovec, account.rs:775-777).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Запас сверх `timeout` длинного опроса: Telegram держит запрос до `timeout` секунд.
pub const LONG_POLL_GRACE: Duration = Duration::from_secs(10);

/// Клиент Bot API. Дешёвый `Clone`: пул соединений общий.
#[derive(Clone)]
pub struct BotApi {
    http: reqwest::Client,
    base_url: String,
    token: SecretString,
    timeout: Duration,
}

impl fmt::Debug for BotApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BotApi")
            .field("base_url", &self.base_url)
            .field("token", &"[REDACTED]")
            .field("timeout", &self.timeout)
            .finish()
    }
}

pub struct BotApiBuilder {
    token: SecretString,
    base_url: String,
    timeout: Duration,
    connect_timeout: Duration,
    proxy: Option<SecretString>,
}

impl fmt::Debug for BotApiBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BotApiBuilder")
            .field("base_url", &self.base_url)
            .field("timeout", &self.timeout)
            .field("proxy", &self.proxy.as_ref().map(|_| "[REDACTED]"))
            .finish_non_exhaustive()
    }
}

impl BotApiBuilder {
    /// Другой адрес сервера: локальный Bot API server или тестовый HTTP-сервер.
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Прокси для всех запросов (`http://`, `https://`; в URL могут быть логин и пароль —
    /// поэтому секрет).
    pub fn proxy(mut self, url: SecretString) -> Self {
        self.proxy = Some(url);
        self
    }

    pub fn build(self) -> Result<BotApi, Error> {
        if !token_looks_valid(self.token.expose_secret()) {
            return Err(Error::Config("malformed bot token".into()));
        }
        let base_url = self.base_url.trim_end_matches('/').to_owned();
        if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
            return Err(Error::Config(
                "base URL must start with http:// or https://".into(),
            ));
        }

        let mut builder = reqwest::Client::builder()
            .tls_backend_preconfigured(tls_config()?)
            .connect_timeout(self.connect_timeout)
            .user_agent(concat!("exch-botapi/", env!("CARGO_PKG_VERSION")));
        builder = match &self.proxy {
            Some(url) => builder.proxy(
                reqwest::Proxy::all(url.expose_secret())
                    .map_err(|_| Error::Config("invalid proxy URL".into()))?,
            ),
            None => builder.no_proxy(),
        };
        let http = builder
            .build()
            .map_err(|e| Error::Config(format!("cannot build HTTP client: {}", e.without_url())))?;

        Ok(BotApi {
            http,
            base_url,
            token: self.token,
            timeout: self.timeout,
        })
    }
}

/// rustls на ring, корни — встроенный набор Mozilla (webpki-roots).
fn tls_config() -> Result<rustls::ClientConfig, Error> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Config(format!("TLS setup failed: {e}")))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(config)
}

/// Токен вида `123456789:AA…`: только ASCII-буквы, цифры, `:`, `_`, `-`. Проверка защищает
/// путь URL от подстановки (`/`, `?`, пробелы) — значение в ошибку не попадает.
fn token_looks_valid(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 256
        && token.contains(':')
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-'))
}

impl BotApi {
    pub fn new(token: SecretString) -> Result<Self, Error> {
        Self::builder(token).build()
    }

    pub fn builder(token: SecretString) -> BotApiBuilder {
        BotApiBuilder {
            token,
            base_url: DEFAULT_BASE_URL.to_owned(),
            timeout: DEFAULT_TIMEOUT,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            proxy: None,
        }
    }

    /// Выполнить типизированный метод.
    pub async fn execute<M: Method>(&self, request: &M) -> Result<M::Response, Error> {
        self.post(M::NAME, request, self.timeout).await
    }

    /// Любой метод Bot API — запасной выход для того, что здесь не описано типами.
    pub async fn call<P, R>(&self, method: &str, params: &P) -> Result<R, Error>
    where
        P: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        self.post(method, params, self.timeout).await
    }

    async fn post<P, R>(&self, method: &str, params: &P, timeout: Duration) -> Result<R, Error>
    where
        P: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let token = self.token.expose_secret();
        let body = serde_json::to_vec(params).map_err(|e| Error::Encode {
            method: method.to_owned(),
            detail: e.to_string(),
        })?;
        let url = format!("{}/bot{token}/{method}", self.base_url);
        tracing::debug!(method, bytes = body.len(), "Bot API: запрос");

        let response = self
            .http
            .post(url)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body(body)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| transport_error(method, e, token))?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| transport_error(method, e, token))?;
        decode_response(method, status, &bytes).map_err(|e| e.scrub(token))
    }

    pub async fn get_me(&self) -> Result<User, Error> {
        self.execute(&GetMe {}).await
    }

    /// Длинный опрос. Ждём ответа `timeout` секунд плюс [`LONG_POLL_GRACE`]. Битое обновление
    /// не ломает порцию: оно приходит как [`Update::skipped`] и сдвигает `offset`.
    pub async fn get_updates(&self, request: &GetUpdates) -> Result<Vec<Update>, Error> {
        let wait = Duration::from_secs(u64::from(request.timeout.unwrap_or(0)));
        let raw: Vec<serde_json::Value> = self
            .post(GetUpdates::NAME, request, wait + LONG_POLL_GRACE)
            .await?;
        let total = raw.len();
        let updates = parse_updates(raw);
        let skipped = updates
            .iter()
            .filter(|u| {
                u.message.is_none() && u.edited_message.is_none() && u.callback_query.is_none()
            })
            .count();
        if updates.len() != total {
            tracing::warn!(
                lost = total - updates.len(),
                "Bot API: обновления без update_id пропущены"
            );
        }
        tracing::debug!(count = updates.len(), skipped, "Bot API: обновления");
        Ok(updates)
    }

    pub async fn send_message(&self, request: &SendMessage) -> Result<Message, Error> {
        self.execute(request).await
    }

    pub async fn send_rich_message(&self, request: &SendRichMessage) -> Result<Message, Error> {
        self.execute(request).await
    }

    /// Правка обычным HTML или rich-разметкой (`EditMessageText::html` / `::rich`).
    pub async fn edit_message_text(&self, request: &EditMessageText) -> Result<Message, Error> {
        self.execute(request).await
    }

    /// Правка rich-сообщения: `editMessageText` с `rich_message {html, skip_entity_detection}`.
    pub async fn edit_message_rich(
        &self,
        chat_id: i64,
        message_id: i32,
        html: impl Into<String>,
        reply_markup: Option<InlineKeyboardMarkup>,
    ) -> Result<Message, Error> {
        let mut request = EditMessageText::rich(chat_id, message_id, html);
        request.reply_markup = reply_markup;
        self.execute(&request).await
    }

    pub async fn answer_callback_query(
        &self,
        request: &AnswerCallbackQuery,
    ) -> Result<bool, Error> {
        self.execute(request).await
    }

    pub async fn delete_message(&self, chat_id: i64, message_id: i32) -> Result<bool, Error> {
        self.execute(&DeleteMessage {
            chat_id,
            message_id,
        })
        .await
    }

    pub async fn set_my_commands(&self, commands: Vec<BotCommand>) -> Result<bool, Error> {
        self.execute(&SetMyCommands::new(commands)).await
    }
}

fn transport_error(method: &str, e: reqwest::Error, token: &str) -> Error {
    let kind = if e.is_connect() {
        TransportKind::Connect
    } else if e.is_timeout() {
        TransportKind::Timeout
    } else {
        TransportKind::Other
    };
    let e = e.without_url();
    let mut detail = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(cause) = source {
        detail.push_str(": ");
        detail.push_str(&cause.to_string());
        source = cause.source();
    }
    Error::Transport {
        method: method.to_owned(),
        kind,
        detail: scrub(&detail, token),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_shape_is_checked_without_echoing_it() {
        assert!(token_looks_valid(
            "123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw"
        ));
        for bad in ["", "no-colon", "1:a/b", "1:a b", "1:a?x", "1:a\n"] {
            assert!(!token_looks_valid(bad), "{bad:?}");
        }
        let err = BotApi::new(SecretString::from("1:a/../../x")).unwrap_err();
        assert!(!err.to_string().contains("../"), "{err}");
    }

    #[test]
    fn base_url_must_be_http() {
        let err = BotApi::builder(SecretString::from("1:abc"))
            .base_url("ftp://example.org")
            .build()
            .unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn debug_never_shows_the_token() {
        let token = "123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw";
        let api = BotApi::new(SecretString::from(token)).unwrap();
        let text = format!("{api:?}");
        assert!(!text.contains(token), "{text}");
        assert!(text.contains("REDACTED"));
    }
}
