//! Вопросы в окне программы для `exch setup`: строка, скрытый ввод, да/нет. Чтение stdin —
//! в `spawn_blocking`, чтобы не держать рантайм.

use std::io::Write as _;

use anyhow::Context as _;
use secrecy::SecretString;

fn show(text: &str) {
    let mut out = std::io::stdout();
    let _ = write!(out, "{text}");
    let _ = out.flush();
}

/// Строка (ввод виден). Пустой ответ — `default`, если он есть.
pub async fn line(question: &str, default: Option<&str>) -> anyhow::Result<String> {
    match default {
        Some(d) if !d.is_empty() => show(&format!("{question} [{d}]: ")),
        _ => show(&format!("{question}: ")),
    }
    let answer = tokio::task::spawn_blocking(|| {
        let mut s = String::new();
        match std::io::stdin().read_line(&mut s) {
            Ok(0) => Err(anyhow::anyhow!("ввод закрыт")),
            Ok(_) => Ok(s.trim().to_owned()),
            Err(e) => Err(e.into()),
        }
    })
    .await
    .context("ввод прерван")??;
    Ok(if answer.is_empty() {
        default.unwrap_or_default().to_owned()
    } else {
        answer
    })
}

/// Секрет: ввод не виден (пароль, токен, ключ). Вставка через правый щелчок работает.
/// Окно без скрытого ввода (некоторые терминалы, ввод из файла) — обычная строка.
pub async fn hidden(question: &str) -> anyhow::Result<SecretString> {
    show(&format!("{question} (ввод не виден): "));
    match mtproto::login::ask_hidden("").await {
        Ok(s) => Ok(s),
        Err(_) => {
            println!();
            let v = line(
                &format!("{question} (это окно не умеет скрывать ввод)"),
                None,
            )
            .await?;
            Ok(SecretString::from(v))
        }
    }
}

/// Да или нет; Enter — `default`.
pub async fn yes(question: &str, default: bool) -> anyhow::Result<bool> {
    let hint = if default { "Д/н" } else { "д/Н" };
    loop {
        let a = line(&format!("{question} [{hint}]"), None).await?;
        match a.to_lowercase().as_str() {
            "" => return Ok(default),
            "д" | "да" | "y" | "yes" => return Ok(true),
            "н" | "нет" | "n" | "no" => return Ok(false),
            _ => println!("Ответьте «д» (да) или «н» (нет)."),
        }
    }
}

/// Сколько окно ждёт Enter, прежде чем закрыться само.
pub const PAUSE_LIMIT: std::time::Duration = std::time::Duration::from_secs(600);

/// Чем кончилась пауза.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseEnd {
    /// Ввода нет (служба, перенаправленный ввод): ждать некого, выходим сразу.
    Skipped,
    /// Нажали Enter (или ввод закрылся).
    Answered,
    /// Никто не ответил за отведённое время.
    TimedOut,
}

/// Пауза перед закрытием окна (запуск двойным щелчком: иначе ошибку не успеть прочитать).
/// Только при живом терминале и не дольше [`PAUSE_LIMIT`]: под службой или в скрытом окне
/// планировщика процесс с ошибкой старта не должен висеть вечно — он завершается с кодом ошибки,
/// и служба запускает его заново (находка Н-2, шаг 2.2).
pub fn pause() -> PauseEnd {
    use std::io::IsTerminal;
    let terminal = std::io::stdin().is_terminal();
    if terminal {
        show("\nНажмите Enter, чтобы закрыть окно (закроется само через 10 минут)…");
    }
    pause_with(terminal, PAUSE_LIMIT, || {
        let mut s = String::new();
        let _ = std::io::stdin().read_line(&mut s);
    })
}

/// [`pause`] без терминала: что читать и сколько ждать — снаружи (для тестов).
pub fn pause_with(
    terminal: bool,
    limit: std::time::Duration,
    read: impl FnOnce() + Send + 'static,
) -> PauseEnd {
    if !terminal {
        return PauseEnd::Skipped;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    // Поток чтения не держит выход: процесс завершится, даже если он ещё ждёт ввода.
    std::thread::spawn(move || {
        read();
        let _ = tx.send(());
    });
    match rx.recv_timeout(limit) {
        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => PauseEnd::Answered,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => PauseEnd::TimedOut,
    }
}

#[cfg(test)]
mod pause_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn without_a_terminal_there_is_no_pause() {
        let started = Instant::now();
        let end = pause_with(false, Duration::from_secs(600), || {
            std::thread::sleep(Duration::from_secs(3600));
        });
        assert_eq!(end, PauseEnd::Skipped);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_pause_nobody_answers_ends_by_itself() {
        let end = pause_with(true, Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_secs(3600));
        });
        assert_eq!(end, PauseEnd::TimedOut);
    }

    #[test]
    fn enter_closes_the_window() {
        assert_eq!(
            pause_with(true, Duration::from_secs(5), || {}),
            PauseEnd::Answered
        );
    }
}
