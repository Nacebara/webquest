//! Старт без присмотра (находка Н-3, шаг 2.3 плана): после перезагрузки сервера PostgreSQL и сеть
//! поднимаются десятки секунд. exch не выходит с ошибкой, а ждёт их — с нарастающей паузой, до
//! [`WAIT_LIMIT`]. Повторяются только временные сбои (нет связи, таймаут, база ещё запускается);
//! неверный пароль, «нужен вход юзербота» и ошибки настроек останавливают старт сразу: ожидание
//! их не исправит, а человек должен увидеть причину.

use std::future::Future;
use std::time::Duration;

/// Сколько ждать базу и Telegram при старте.
pub const WAIT_LIMIT: Duration = Duration::from_secs(600);
/// Самая длинная пауза между попытками.
const MAX_PAUSE: Duration = Duration::from_secs(30);

/// Пауза перед попыткой `n` (с нуля): 1, 2, 4, 8, 16, 30, 30… с.
pub fn pause_before(n: u32) -> Duration {
    Duration::from_secs(1u64 << n.min(5)).min(MAX_PAUSE)
}

/// Выполнять `op`, пока она не удастся, ошибка не окажется постоянной (`transient` — `false`)
/// или не выйдет `limit`. Возвращает последнюю ошибку.
pub async fn retry<T, E, Fut>(
    what: &str,
    limit: Duration,
    mut op: impl FnMut() -> Fut,
    transient: impl Fn(&E) -> bool,
) -> Result<T, E>
where
    Fut: Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let started = tokio::time::Instant::now();
    let mut attempt = 0u32;
    loop {
        match op().await {
            Ok(v) => {
                if attempt > 0 {
                    tracing::info!(what, attempts = attempt + 1, "ready after waiting");
                }
                return Ok(v);
            }
            Err(e) if transient(&e) => {
                let pause = pause_before(attempt);
                if started.elapsed() + pause > limit {
                    tracing::error!(what, error = %e, waited_secs = started.elapsed().as_secs(), "gave up waiting");
                    return Err(e);
                }
                tracing::warn!(what, error = %e, attempt = attempt + 1, next_in_secs = pause.as_secs(), "not ready yet, waiting");
                tokio::time::sleep(pause).await;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Временный ли сбой базы: нет связи, таймаут, сервер запускается или перегружен. Неверный
/// пароль, нет прав, ошибка в адресе — нет.
pub fn transient_db(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed => true,
        // 57P03 — «база запускается», 08… — связь, 53… — нехватка ресурсов (соединений).
        sqlx::Error::Database(db) => db
            .code()
            .is_some_and(|c| c == "57P03" || c.starts_with("08") || c.starts_with("53")),
        _ => false,
    }
}

/// То же для ошибки с контекстом: ищет в цепочке ошибку базы.
pub fn transient_db_chain(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<sqlx::Error>().is_some_and(transient_db)
            || c.downcast_ref::<storage::StorageError>()
                .is_some_and(|s| matches!(s, storage::StorageError::Db(e) if transient_db(e)))
    })
}

/// Временный ли сбой подключения юзербота: сеть или таймаут. «Нет сессии», «вход сброшен»,
/// ошибка проверки ботов кошельков — нет.
pub fn transient_telegram(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<mtproto::ConnectError>().is_some_and(|c| {
            matches!(
                c,
                mtproto::ConnectError::Timeout(_) | mtproto::ConnectError::Network { .. }
            )
        })
    }) || transient_db_chain(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn pauses_grow_and_stop_at_thirty_seconds() {
        let p: Vec<u64> = (0..8).map(|n| pause_before(n).as_secs()).collect();
        assert_eq!(p, [1, 2, 4, 8, 16, 30, 30, 30]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_database_that_comes_up_later_is_waited_for() {
        let calls = AtomicU32::new(0);
        let r: Result<&str, String> = retry(
            "database",
            WAIT_LIMIT,
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n < 4 {
                        Err("connection refused".to_owned())
                    } else {
                        Ok("up")
                    }
                }
            },
            |_| true,
        )
        .await;
        assert_eq!(r, Ok("up"));
        assert_eq!(calls.load(Ordering::SeqCst), 5);
    }

    #[tokio::test(start_paused = true)]
    async fn a_permanent_error_stops_the_start_at_once() {
        let calls = AtomicU32::new(0);
        let r: Result<(), String> = retry(
            "database",
            WAIT_LIMIT,
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err("password authentication failed".to_owned()) }
            },
            |e| !e.contains("password"),
        )
        .await;
        assert!(r.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn waiting_ends_after_ten_minutes() {
        let started = tokio::time::Instant::now();
        let r: Result<(), String> = retry(
            "database",
            WAIT_LIMIT,
            || async { Err("connection refused".to_owned()) },
            |_| true,
        )
        .await;
        assert!(r.is_err());
        let waited = started.elapsed();
        assert!(waited <= WAIT_LIMIT, "{waited:?}");
        assert!(waited >= WAIT_LIMIT - MAX_PAUSE, "{waited:?}");
    }

    #[test]
    fn refused_connection_is_temporary_bad_address_is_not() {
        let io = sqlx::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        assert!(transient_db(&io));
        assert!(!transient_db(&sqlx::Error::Configuration("bad url".into())));
        let chained = anyhow::Error::new(io).context("cannot connect to PostgreSQL");
        assert!(transient_db_chain(&chained));
    }
}
