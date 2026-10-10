//! Ошибки grammers → `TransportError` (DESIGN v0.2 §2, LOVEC-PORTING §3.3, урок M1).
//!
//! Главное правило: всё, после чего запрос МОГ выполниться на сервере, — исход неизвестен
//! (`Timeout` / `Disconnected`, у них `outcome_unknown() == true`). Повтор допустим только с тем
//! же `random_id`; нажатие кнопки не повторяется вовсе.
//!
//! | grammers | `TransportError` | почему |
//! |---|---|---|
//! | `Rpc RANDOM_ID_DUPLICATE` (любой код) | `RandomIdDuplicate` | сообщение с этим `random_id` уже создано |
//! | `Rpc 420 FLOOD_WAIT_X`, `FLOOD_PREMIUM_WAIT_X`, `SLOWMODE_WAIT_X` | `FloodWait(X s)` | явный отказ, ждать X |
//! | `Rpc 401` и `AUTH_KEY_*`, `SESSION_*`, `USER_DEACTIVATED` | `NotAuthorized` | нужен `exch login` |
//! | `Rpc BOT_RESPONSE_TIMEOUT` | `Timeout` | бот мог обработать нажатие, просто не ответил вовремя |
//! | `Rpc` с кодом ≥ 500 или < 0 (`-503 Timeout`, `RPC_CALL_FAIL`…) | `Timeout` | сервер мог выполнить запрос |
//! | прочие `Rpc` 3xx/4xx (`PEER_FLOOD`, `QUERY_ID_INVALID`…) | `Rpc { code, name }` | явный отказ, запрос не выполнен |
//! | `Io`, `Transport` (bad CRC, seq, длина), `Dropped` | `Disconnected` | соединение оборвалось, запрос мог уйти |
//! | `Deserialize` | `Timeout` | ответ пришёл, но не разобран — запрос выполнен или нет, неизвестно |
//! | `InvalidDc`, `Session`, `Authentication` | `Other` | запрос не отправлялся |
//! | истёк `tokio::time::timeout` | `Timeout` | |

use std::time::Duration;

use grammers_mtsender::{InvocationError, RpcError};
use userbot::TransportError;

/// Если сервер не сказал, сколько ждать, — ждём минуту (как lovec `flood.rs`).
pub const DEFAULT_FLOOD_WAIT: Duration = Duration::from_secs(60);

const FLOOD_NAMES: [&str; 3] = ["FLOOD_WAIT", "FLOOD_PREMIUM_WAIT", "SLOWMODE_WAIT"];

/// Ошибки, после которых сессия недействительна. Имена сравниваются точно (AUDIT-2.md:1055).
const AUTH_NAMES: [&str; 8] = [
    "AUTH_KEY_UNREGISTERED",
    "AUTH_KEY_INVALID",
    "AUTH_KEY_PERM_EMPTY",
    "AUTH_KEY_DUPLICATED",
    "SESSION_REVOKED",
    "SESSION_EXPIRED",
    "SESSION_PASSWORD_NEEDED",
    "USER_DEACTIVATED",
];

/// Явные ответы сервера, при которых действие могло быть выполнено.
const UNKNOWN_OUTCOME_NAMES: [&str; 1] = ["BOT_RESPONSE_TIMEOUT"];

fn flood_wait_of(rpc: &RpcError) -> Option<Duration> {
    FLOOD_NAMES.contains(&rpc.name.as_str()).then(|| {
        rpc.value
            .map(|s| Duration::from_secs(u64::from(s).max(1)))
            .unwrap_or(DEFAULT_FLOOD_WAIT)
    })
}

/// Ошибка означает, что аккаунт разлогинен или заблокирован.
pub fn is_auth_error(error: &InvocationError) -> bool {
    match error {
        InvocationError::Rpc(rpc) => {
            rpc.code == 401
                || AUTH_NAMES.contains(&rpc.name.as_str())
                || rpc.name == "USER_DEACTIVATED_BAN"
        }
        _ => false,
    }
}

/// Перевести ошибку grammers в ошибку контракта транспорта.
pub fn map_invocation_error(error: &InvocationError) -> TransportError {
    match error {
        InvocationError::Rpc(rpc) => map_rpc(rpc),
        InvocationError::Io(_) | InvocationError::Transport(_) | InvocationError::Dropped => {
            TransportError::Disconnected
        }
        InvocationError::Deserialize(_) => TransportError::Timeout,
        InvocationError::InvalidDc => {
            TransportError::Other("invalid datacenter; request was not sent".into())
        }
        InvocationError::Session(_) => {
            TransportError::Other("session storage error; request was not sent".into())
        }
        InvocationError::Authentication(_) => TransportError::Other(
            "authorization key generation failed; request was not sent".into(),
        ),
    }
}

fn map_rpc(rpc: &RpcError) -> TransportError {
    if rpc.name == "RANDOM_ID_DUPLICATE" {
        return TransportError::RandomIdDuplicate;
    }
    if let Some(wait) = flood_wait_of(rpc) {
        return TransportError::FloodWait(wait);
    }
    if rpc.code == 401
        || AUTH_NAMES.contains(&rpc.name.as_str())
        || rpc.name == "USER_DEACTIVATED_BAN"
    {
        return TransportError::NotAuthorized;
    }
    if UNKNOWN_OUTCOME_NAMES.contains(&rpc.name.as_str()) || rpc.code >= 500 || rpc.code < 0 {
        return TransportError::Timeout;
    }
    TransportError::Rpc {
        code: rpc.code,
        name: rpc.name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammers_tl_types as tl;

    fn rpc(code: i32, message: &str) -> InvocationError {
        InvocationError::Rpc(RpcError::from(tl::types::RpcError {
            error_code: code,
            error_message: message.into(),
        }))
    }

    #[test]
    fn flood_waits() {
        assert_eq!(
            map_invocation_error(&rpc(420, "FLOOD_WAIT_31")),
            TransportError::FloodWait(Duration::from_secs(31))
        );
        assert_eq!(
            map_invocation_error(&rpc(420, "FLOOD_PREMIUM_WAIT_703")),
            TransportError::FloodWait(Duration::from_secs(703)),
            "no upper clamp: waking earlier only extends the ban"
        );
        assert_eq!(
            map_invocation_error(&rpc(420, "SLOWMODE_WAIT_10")),
            TransportError::FloodWait(Duration::from_secs(10))
        );
        assert_eq!(
            map_invocation_error(&rpc(420, "FLOOD_WAIT_0")),
            TransportError::FloodWait(Duration::from_secs(1))
        );
        assert_eq!(
            map_invocation_error(&InvocationError::Rpc(RpcError {
                code: 420,
                name: "FLOOD_WAIT".into(),
                value: None,
                caused_by: None,
            })),
            TransportError::FloodWait(DEFAULT_FLOOD_WAIT)
        );
    }

    #[test]
    fn random_id_duplicate_regardless_of_code() {
        for code in [400, 500] {
            let e = map_invocation_error(&rpc(code, "RANDOM_ID_DUPLICATE"));
            assert_eq!(e, TransportError::RandomIdDuplicate);
            assert!(!e.outcome_unknown(), "duplicate means: already delivered");
        }
    }

    #[test]
    fn auth_errors() {
        for (code, name) in [
            (401, "AUTH_KEY_UNREGISTERED"),
            (401, "SESSION_REVOKED"),
            (401, "SESSION_EXPIRED"),
            (401, "USER_DEACTIVATED"),
            (406, "AUTH_KEY_DUPLICATED"),
            (401, "SOMETHING_NEW"),
        ] {
            let e = rpc(code, name);
            assert!(is_auth_error(&e), "{name}");
            assert_eq!(
                map_invocation_error(&e),
                TransportError::NotAuthorized,
                "{name}"
            );
        }
        assert!(!is_auth_error(&rpc(400, "PEER_FLOOD")));
        assert!(!is_auth_error(&InvocationError::Dropped));
    }

    #[test]
    fn server_side_failures_are_outcome_unknown() {
        for e in [
            rpc(500, "RPC_CALL_FAIL"),
            rpc(500, "INTERDC_2_CALL_ERROR"),
            rpc(-503, "Timeout"),
            rpc(400, "BOT_RESPONSE_TIMEOUT"),
        ] {
            let mapped = map_invocation_error(&e);
            assert_eq!(mapped, TransportError::Timeout, "{e}");
            assert!(mapped.outcome_unknown());
        }
    }

    #[test]
    fn network_failures_are_outcome_unknown() {
        let io = InvocationError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "reset",
        ));
        let crc = InvocationError::Transport(grammers_mtproto::transport::Error::BadCrc {
            expected: 1,
            got: 2,
        });
        for e in [io, crc, InvocationError::Dropped] {
            let mapped = map_invocation_error(&e);
            assert_eq!(mapped, TransportError::Disconnected, "{e}");
            assert!(mapped.outcome_unknown());
        }
        let bad_body = InvocationError::Deserialize(
            grammers_mtproto::mtp::DeserializeError::MessageBufferTooSmall,
        );
        assert!(map_invocation_error(&bad_body).outcome_unknown());
    }

    #[test]
    fn explicit_rejections_keep_name() {
        for (code, name) in [
            (400, "PEER_FLOOD"),
            (400, "QUERY_ID_INVALID"),
            (400, "RESULT_ID_INVALID"),
            (400, "MESSAGE_ID_INVALID"),
            (400, "DATA_INVALID"),
            (403, "CHAT_WRITE_FORBIDDEN"),
        ] {
            let mapped = map_invocation_error(&rpc(code, name));
            assert_eq!(
                mapped,
                TransportError::Rpc {
                    code,
                    name: name.into()
                }
            );
            assert!(!mapped.outcome_unknown());
        }
    }

    #[test]
    fn not_sent_errors() {
        let mapped = map_invocation_error(&InvocationError::InvalidDc);
        assert!(matches!(mapped, TransportError::Other(_)));
        assert!(!mapped.outcome_unknown());
        let session = InvocationError::Session("poisoned".into());
        assert!(matches!(
            map_invocation_error(&session),
            TransportError::Other(_)
        ));
    }
}
