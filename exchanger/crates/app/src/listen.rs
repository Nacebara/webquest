//! `exch listen --out <файл.jsonl> [--history N]` — слушатель для ноутбука владельца
//! (docs/RECORDING.md): записать настоящие ответы @send и @xrocket без базы данных и без сервера.
//!
//! Отличия от `exch record`:
//! - нет PostgreSQL: сообщения пишутся только в файл, `wallet_messages` не трогается;
//! - вход по номеру, коду и облачному паролю — прямо здесь, в терминале;
//! - сессия живёт только в памяти под случайным ключом этого процесса: на диск не попадает,
//!   ключ нигде не хранится; на выходе — `auth.logOut`, в «Устройствах» Telegram сессии не остаётся;
//! - `api_id` и `api_hash` — из `TG_API_ID` / `TG_API_HASH` (или `.env`), иначе спрашиваются;
//!   `api_hash` вводится без эха.
//!
//! Ничего не отправляет и не нажимает: только подписка на сообщения и чтение истории.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use mtproto::{
    GrammersTransport, MemoryStore, MtprotoConfig, PinnedBots, SessionKey, TerminalPrompt,
};
use secrecy::{ExposeSecret, SecretString};
use tokio_util::sync::CancellationToken;

/// Метка аккаунта в логах и в строках файла.
pub const LABEL: &str = "listen";

/// `api_id` — положительное число из my.telegram.org.
pub fn parse_api_id(raw: &str) -> anyhow::Result<i32> {
    let id: i32 = raw
        .trim()
        .parse()
        .context("api_id — это число из my.telegram.org")?;
    anyhow::ensure!(id > 0, "api_id должен быть больше нуля");
    Ok(id)
}

/// Значение из окружения, если задано и не пустое.
fn from_env(var: &str) -> Option<String> {
    std::env::var(var)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

async fn api_credentials() -> anyhow::Result<(i32, SecretString)> {
    let api_id = match from_env("TG_API_ID") {
        Some(v) => v,
        None => mtproto::ask("api_id (my.telegram.org → API development tools): ")
            .await?
            .expose_secret()
            .to_owned(),
    };
    let api_id = parse_api_id(&api_id)?;
    let api_hash = match from_env("TG_API_HASH") {
        Some(v) => SecretString::from(v),
        None => mtproto::ask_hidden("api_hash (ввод не виден): ").await?,
    };
    anyhow::ensure!(!api_hash.expose_secret().is_empty(), "api_hash пустой");
    Ok((api_id, api_hash))
}

fn say(line: &str) {
    eprintln!("{line}");
}

/// Войти, записывать до Ctrl-C, выйти из аккаунта.
///
/// Ctrl-C перехватывается только после входа: во время вопросов о номере и коде он закрывает
/// программу как обычно (убирать ещё нечего), а после — останавливает запись и выходит из аккаунта.
pub async fn run(out: &Path, history: u32) -> anyhow::Result<()> {
    let (api_id, api_hash) = api_credentials().await?;
    let mut cfg = MtprotoConfig::new(LABEL, api_id, api_hash);
    cfg.bots = PinnedBots::default();
    let key = SessionKey::ephemeral().context("не удалось создать ключ сессии")?;
    let store = Arc::new(MemoryStore::new(None));

    say("Вход в тестовый аккаунт. Номер, код и пароль никуда не сохраняются.");
    let outcome = mtproto::login(&cfg, &key, store.clone(), &mut TerminalPrompt)
        .await
        .context("вход не удался")?;
    say(&format!("Вход выполнен: {}.", outcome.phone_masked));

    let transport = GrammersTransport::connect(cfg, key.duplicate(), store)
        .await
        .context("не удалось подключиться к Telegram")?;
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        stop.cancel();
    });
    let result = record(&transport, out, history, &cancel).await;

    // Сессия была только в памяти — убрать её и у Telegram, при любом исходе записи.
    match transport.log_out().await {
        Ok(()) => say("Вышел из аккаунта: в «Устройствах» Telegram этой сессии больше нет."),
        Err(e) => say(&format!(
            "Не удалось выйти из аккаунта ({e}). Завершите сессию «exch» вручную: Настройки → Устройства."
        )),
    }
    transport.shutdown().await;
    result
}

async fn record(
    transport: &GrammersTransport,
    out: &Path,
    history: u32,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    let mut file = crate::record::open_output(out)
        .await
        .with_context(|| format!("не удалось открыть файл {}", out.display()))?;
    let log = userbot::flows::NoLog;
    let rx = userbot::Transport::subscribe(transport);
    let backfilled = crate::record::backfill(transport, history, &mut file, &log, LABEL).await?;
    say(&format!(
        "Записываю в {}. Из истории чатов взято {backfilled} сообщений.",
        out.display()
    ));
    say("Ничего не отправляю и не нажимаю. Делайте шаги из инструкции; остановить — Ctrl-C.");
    let stats = crate::record::record_stream(rx, &mut file, &log, LABEL, cancel).await?;
    say(&format!(
        "Запись остановлена. Новых сообщений: {}.",
        stats.written
    ));
    if stats.lagged > 0 {
        say(&format!(
            "Пропущено {} сообщений: запись не успевала. Запустите ещё раз с --history 300.",
            stats.lagged
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_id_is_a_positive_number() {
        assert_eq!(parse_api_id(" 1234567 ").unwrap(), 1_234_567);
        assert!(parse_api_id("0").is_err());
        assert!(parse_api_id("-5").is_err());
        assert!(parse_api_id("abc").is_err());
        assert!(parse_api_id("").is_err());
    }
}
