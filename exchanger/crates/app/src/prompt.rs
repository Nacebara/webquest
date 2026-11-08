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

/// Пауза перед закрытием окна (запуск двойным щелчком: иначе ошибку не успеть прочитать).
pub fn pause() {
    show("\nНажмите Enter, чтобы закрыть окно…");
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
}
