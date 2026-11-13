//! Сигналы остановки и время на мягкую остановку (находка Н-1, шаг 2.1 плана).
//!
//! Linux: SIGTERM и Ctrl+C. Windows: Ctrl+C и Ctrl+Break (так останавливает консольную программу
//! служба-обёртка), закрытие окна, выход из системы и выключение компьютера. На последние три
//! Windows ждёт программу всего около 5 секунд и потом завершает её сама (tokio держит процесс,
//! пока мы не выйдем, но не дольше этого окна), поэтому для них окно остановки короткое: успеть
//! записать «stopped» и закрыть соединения. Работа, прерванная на середине, продолжается при
//! следующем запуске (восстановление операций в движке).

use std::time::Duration;

/// Сколько ждать задачи после обычного сигнала остановки. Таймаут службы — не меньше 70 с.
pub const GRACE: Duration = Duration::from_secs(60);
/// Сколько ждать задачи, когда Windows закрывает окно, завершает сеанс или выключается: она
/// даёт консольной программе около 5 с.
pub const WINDOWS_GRACE: Duration = Duration::from_secs(4);

/// Что попросило программу остановиться.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopSignal {
    /// Ctrl+C.
    Interrupt,
    /// SIGTERM (Linux) или Ctrl+Break (Windows, служба-обёртка).
    Terminate,
    /// Закрыли окно консоли.
    ConsoleClosed,
    /// Пользователь вышел из системы.
    Logoff,
    /// Компьютер выключается или перезагружается.
    SystemShutdown,
}

impl StopSignal {
    /// Сколько есть времени на мягкую остановку.
    pub fn grace(self) -> Duration {
        match self {
            StopSignal::Interrupt | StopSignal::Terminate => GRACE,
            StopSignal::ConsoleClosed | StopSignal::Logoff | StopSignal::SystemShutdown => {
                WINDOWS_GRACE
            }
        }
    }

    /// Имя для журнала.
    pub fn name(self) -> &'static str {
        match self {
            StopSignal::Interrupt => "interrupt",
            StopSignal::Terminate => "terminate",
            StopSignal::ConsoleClosed => "console closed",
            StopSignal::Logoff => "logoff",
            StopSignal::SystemShutdown => "system shutdown",
        }
    }
}

/// Дождаться сигнала остановки. Не удалось подписаться на сигнал — остаётся Ctrl+C.
pub async fn wait() -> StopSignal {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = term.recv() => StopSignal::Terminate,
                    _ = tokio::signal::ctrl_c() => StopSignal::Interrupt,
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot listen for SIGTERM, falling back to Ctrl-C");
                let _ = tokio::signal::ctrl_c().await;
                StopSignal::Interrupt
            }
        }
    }
    #[cfg(windows)]
    {
        windows_wait().await
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = tokio::signal::ctrl_c().await;
        StopSignal::Interrupt
    }
}

#[cfg(windows)]
async fn windows_wait() -> StopSignal {
    use tokio::signal::windows::{ctrl_break, ctrl_close, ctrl_logoff, ctrl_shutdown};
    // Подписка не удалась — этот сигнал ждём «никогда», остальные работают.
    async fn never() -> Option<()> {
        std::future::pending().await
    }
    let mut brk = ctrl_break()
        .inspect_err(|e| tracing::warn!(error = %e, "cannot listen for Ctrl-Break"))
        .ok();
    let mut close = ctrl_close()
        .inspect_err(|e| tracing::warn!(error = %e, "cannot listen for console close"))
        .ok();
    let mut logoff = ctrl_logoff()
        .inspect_err(|e| tracing::warn!(error = %e, "cannot listen for logoff"))
        .ok();
    let mut shutdown = ctrl_shutdown()
        .inspect_err(|e| tracing::warn!(error = %e, "cannot listen for shutdown"))
        .ok();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => StopSignal::Interrupt,
        _ = async { match brk.as_mut() { Some(s) => s.recv().await, None => never().await } } => StopSignal::Terminate,
        _ = async { match close.as_mut() { Some(s) => s.recv().await, None => never().await } } => StopSignal::ConsoleClosed,
        _ = async { match logoff.as_mut() { Some(s) => s.recv().await, None => never().await } } => StopSignal::Logoff,
        _ = async { match shutdown.as_mut() { Some(s) => s.recv().await, None => never().await } } => StopSignal::SystemShutdown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_closing_signals_fit_into_the_five_seconds_windows_gives() {
        for s in [
            StopSignal::ConsoleClosed,
            StopSignal::Logoff,
            StopSignal::SystemShutdown,
        ] {
            assert!(s.grace() < Duration::from_secs(5), "{s:?}");
        }
    }

    #[test]
    fn a_service_stop_gets_the_full_grace_period() {
        // Таймаут службы — не меньше 70 с: задачи успевают закончить начатое.
        for s in [StopSignal::Interrupt, StopSignal::Terminate] {
            assert_eq!(s.grace(), GRACE);
            assert!(s.grace() < Duration::from_secs(70));
        }
    }
}
