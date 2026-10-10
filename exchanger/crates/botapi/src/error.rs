//! Ошибки Bot API и разбор ответа `{ok, result | error_code, description, parameters}`.
//!
//! Токен бота — часть URL запроса. Поэтому ошибки хранят только строки: текст транспортной
//! ошибки берётся после `reqwest::Error::without_url()` и дополнительно вычищается от токена
//! (на случай, если прокси вернул страницу с URL). Исходный `reqwest::Error` не хранится:
//! его цепочку `source` нельзя гарантированно очистить (CLAUDE.md, правило 13).

use std::fmt;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::types::ResponseParameters;

/// Сколько символов тела ответа показывать в ошибке разбора.
const BODY_SNIPPET_CHARS: usize = 300;

/// Чем закончилась транспортная ошибка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    /// Соединение не установилось (DNS, TCP, TLS): запрос до Telegram не дошёл.
    Connect,
    /// Таймаут после отправки: запрос мог выполниться.
    Timeout,
    /// Разрыв или иной сбой после установления соединения: исход неизвестен.
    Other,
}

impl fmt::Display for TransportKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TransportKind::Connect => "connection not established",
            TransportKind::Timeout => "timed out",
            TransportKind::Other => "connection failed",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Telegram разобрал запрос и ответил `ok: false`.
    #[error("Bot API {method}: {description} (error_code {error_code})")]
    Api {
        method: String,
        error_code: i32,
        description: String,
        parameters: ResponseParameters,
    },
    #[error("Bot API {method}: {kind}: {detail}")]
    Transport {
        method: String,
        kind: TransportKind,
        detail: String,
    },
    /// Ответ не похож на ответ Bot API (страница прокси, обрезанное тело, чужой формат).
    #[error("Bot API {method}: unexpected response (HTTP {status}): {detail}")]
    Decode {
        method: String,
        status: u16,
        detail: String,
    },
    #[error("Bot API {method}: cannot encode request: {detail}")]
    Encode { method: String, detail: String },
    /// Неверная настройка клиента. Значения (токен, прокси) в текст не попадают.
    #[error("Bot API client: {0}")]
    Config(String),
}

impl Error {
    /// `error_code` из ответа Telegram.
    pub fn error_code(&self) -> Option<i32> {
        match self {
            Error::Api { error_code, .. } => Some(*error_code),
            _ => None,
        }
    }

    pub fn description(&self) -> Option<&str> {
        match self {
            Error::Api { description, .. } => Some(description),
            _ => None,
        }
    }

    /// Сколько ждать перед повтором (429 Too Many Requests). Клиент сам не ждёт: решает
    /// вызывающий (очередь отправки с лимитом на чат).
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Error::Api { parameters, .. } => parameters.retry_after.map(Duration::from_secs),
            _ => None,
        }
    }

    /// Группа стала супергруппой — новый id чата.
    pub fn migrate_to_chat_id(&self) -> Option<i64> {
        match self {
            Error::Api { parameters, .. } => parameters.migrate_to_chat_id,
            _ => None,
        }
    }

    pub fn is_flood(&self) -> bool {
        self.error_code() == Some(429)
    }

    /// Telegram отказал по существу (4xx, кроме 429). Для rich-экрана — повод отправить
    /// HTML-фолбэк (lovec `is_rejection`).
    pub fn is_rejection(&self) -> bool {
        matches!(self.error_code(), Some(code) if (400..500).contains(&code) && code != 429)
    }

    /// 403: бот заблокирован пользователем или не может писать в чат.
    pub fn is_forbidden(&self) -> bool {
        self.error_code() == Some(403)
    }

    /// Правка ничего не меняет — считать успехом (lovec `EditError::NotModified`).
    pub fn is_not_modified(&self) -> bool {
        self.description_contains(&["message is not modified"])
    }

    /// Сообщения для правки больше нет или его нельзя править (lovec `EditError::Gone`).
    pub fn is_message_gone(&self) -> bool {
        self.description_contains(&[
            "message to edit not found",
            "message can't be edited",
            "message_id_invalid",
            "message not found",
            "message to delete not found",
            "message can't be deleted",
        ])
    }

    /// Запрос точно не дошёл до Telegram — отправить снова безопасно, дубля не будет.
    pub fn not_delivered(&self) -> bool {
        matches!(
            self,
            Error::Transport {
                kind: TransportKind::Connect,
                ..
            } | Error::Encode { .. }
                | Error::Config(_)
        )
    }

    /// Запрос мог выполниться: повтор `sendMessage` может дать дубль сообщения.
    pub fn outcome_unknown(&self) -> bool {
        match self {
            Error::Transport { kind, .. } => *kind != TransportKind::Connect,
            Error::Decode { .. } => true,
            Error::Api { error_code, .. } => *error_code >= 500,
            Error::Encode { .. } | Error::Config(_) => false,
        }
    }

    fn description_contains(&self, needles: &[&str]) -> bool {
        let Some(description) = self.description() else {
            return false;
        };
        let description = description.to_lowercase();
        needles.iter().any(|n| description.contains(n))
    }

    /// Убрать секрет из всех текстов ошибки.
    pub fn scrub(self, secret: &str) -> Self {
        match self {
            Error::Api {
                method,
                error_code,
                description,
                parameters,
            } => Error::Api {
                method: scrub(&method, secret),
                error_code,
                description: scrub(&description, secret),
                parameters,
            },
            Error::Transport {
                method,
                kind,
                detail,
            } => Error::Transport {
                method: scrub(&method, secret),
                kind,
                detail: scrub(&detail, secret),
            },
            Error::Decode {
                method,
                status,
                detail,
            } => Error::Decode {
                method: scrub(&method, secret),
                status,
                detail: scrub(&detail, secret),
            },
            Error::Encode { method, detail } => Error::Encode {
                method: scrub(&method, secret),
                detail: scrub(&detail, secret),
            },
            Error::Config(text) => Error::Config(scrub(&text, secret)),
        }
    }
}

/// Заменить все вхождения секрета на `<redacted>`.
pub fn scrub(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        return text.to_owned();
    }
    text.replace(secret, "<redacted>")
}

#[derive(Deserialize)]
struct Envelope {
    ok: bool,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error_code: Option<i32>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Option<ResponseParameters>,
}

/// Разобрать тело ответа Bot API. Решение принимается по полю `ok`, а не по HTTP-статусу:
/// Telegram отвечает ошибкой с кодом 400/403/429 и JSON-телом.
pub fn decode_response<R: DeserializeOwned>(
    method: &str,
    status: u16,
    body: &[u8],
) -> Result<R, Error> {
    let envelope: Envelope = serde_json::from_slice(body).map_err(|_| Error::Decode {
        method: method.to_owned(),
        status,
        detail: snippet(body),
    })?;
    if envelope.ok {
        let result = envelope.result.unwrap_or(serde_json::Value::Null);
        return serde_json::from_value(result).map_err(|e| Error::Decode {
            method: method.to_owned(),
            status,
            detail: format!("result does not match the expected type: {e}"),
        });
    }
    Err(Error::Api {
        method: method.to_owned(),
        error_code: envelope.error_code.unwrap_or_else(|| i32::from(status)),
        description: envelope.description.unwrap_or_default(),
        parameters: envelope.parameters.unwrap_or_default(),
    })
}

fn snippet(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let text = text.trim();
    match text.char_indices().nth(BODY_SNIPPET_CHARS) {
        Some((idx, _)) => format!("{}…", &text[..idx]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::User;

    #[test]
    fn ok_result_is_decoded() {
        let user: User = decode_response(
            "getMe",
            200,
            br#"{"ok":true,"result":{"id":42,"is_bot":true,"first_name":"exch","username":"exch_bot","can_join_groups":false}}"#,
        )
        .unwrap();
        assert_eq!(user.id, 42);
        assert_eq!(user.username.as_deref(), Some("exch_bot"));
    }

    #[test]
    fn telegram_error_keeps_code_description_and_retry_after() {
        let err = decode_response::<bool>(
            "sendMessage",
            429,
            br#"{"ok":false,"error_code":429,"description":"Too Many Requests: retry after 7","parameters":{"retry_after":7}}"#,
        )
        .unwrap_err();
        assert!(err.is_flood());
        assert!(!err.is_rejection());
        assert_eq!(err.retry_after(), Some(Duration::from_secs(7)));
        assert_eq!(
            err.to_string(),
            "Bot API sendMessage: Too Many Requests: retry after 7 (error_code 429)"
        );
    }

    #[test]
    fn edit_errors_are_classified() {
        let not_modified = decode_response::<bool>(
            "editMessageText",
            400,
            br#"{"ok":false,"error_code":400,"description":"Bad Request: message is not modified: specified new message content and reply markup are exactly the same"}"#,
        )
        .unwrap_err();
        assert!(not_modified.is_not_modified());
        assert!(not_modified.is_rejection());
        assert!(!not_modified.is_message_gone());

        let gone = decode_response::<bool>(
            "editMessageText",
            400,
            br#"{"ok":false,"error_code":400,"description":"Bad Request: message to edit not found"}"#,
        )
        .unwrap_err();
        assert!(gone.is_message_gone());
        assert!(!gone.outcome_unknown());

        let blocked = decode_response::<bool>(
            "sendMessage",
            403,
            br#"{"ok":false,"error_code":403,"description":"Forbidden: bot was blocked by the user"}"#,
        )
        .unwrap_err();
        assert!(blocked.is_forbidden());
    }

    #[test]
    fn non_json_body_is_a_decode_error_with_clipped_snippet() {
        let page = format!("<html>{}</html>", "x".repeat(1000));
        let err = decode_response::<bool>("getMe", 502, page.as_bytes()).unwrap_err();
        let Error::Decode { status, detail, .. } = &err else {
            panic!("ожидалась ошибка разбора: {err:?}");
        };
        assert_eq!(*status, 502);
        assert!(detail.chars().count() <= BODY_SNIPPET_CHARS + 1);
        assert!(err.outcome_unknown());
    }

    #[test]
    fn missing_error_code_falls_back_to_http_status() {
        let err =
            decode_response::<bool>("getMe", 404, br#"{"ok":false,"description":"Not Found"}"#)
                .unwrap_err();
        assert_eq!(err.error_code(), Some(404));
    }

    #[test]
    fn scrub_removes_every_occurrence() {
        let token = "123456:ABC-def";
        let err = Error::Decode {
            method: "getMe".into(),
            status: 502,
            detail: format!("GET /bot{token}/getMe failed; again {token}"),
        }
        .scrub(token);
        let text = format!("{err} {err:?}");
        assert!(!text.contains(token), "{text}");
        assert!(text.contains("<redacted>"));
    }
}
