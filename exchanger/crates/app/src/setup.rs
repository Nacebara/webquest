//! `exch setup` — мастер первой настройки по-русски, как у гаранта: всё по вопросам, с
//! проверкой каждого ответа. База PostgreSQL — по паролю пользователя `postgres` (база `exch`
//! создаётся сама), бот — по токену (проверка getMe), ID владельца — из сообщения боту,
//! ключ шифрования сессий — создаётся сам, вход юзерботом — сразу для обоих кошельков.
//!
//! Секреты пишутся только в `.env` (прежний — в `.env.bak`) и на экран не выводятся. Каждый
//! шаг сохраняется сразу: прервали — следующий запуск продолжит с того же места.

use std::time::{Duration, Instant};

use anyhow::Context as _;
use base64::Engine as _;
use botapi::{BotApi, ChatType, GetUpdates};
use domain::Platform;
use secrecy::{ExposeSecret as _, SecretString};
use storage::{PgPool, sessions, wallets};

use crate::config::Config;
use crate::db;
use crate::login::{LoginTarget, MtprotoAuthenticator, run_login};
use crate::project::{EnvFile, Paths};
use crate::prompt;
use crate::settings_file;
use crate::telegram::TelegramSettings;

/// Шаблон `.env`, если файла ещё нет: комментарии подсказывают, что где.
pub const ENV_EXAMPLE: &str = include_str!("../../../.env.example");

/// Сколько ждать сообщения владельца боту.
const OWNER_WAIT: Duration = Duration::from_secs(120);

/// Название кошелька для людей.
pub fn platform_name(p: Platform) -> &'static str {
    match p {
        Platform::CryptoBot => "CryptoBot",
        Platform::XRocket => "xRocket",
    }
}

struct Wizard<'a> {
    paths: &'a Paths,
    env: EnvFile,
}

impl Wizard<'_> {
    fn save(&self) -> anyhow::Result<()> {
        std::fs::write(&self.paths.env, self.env.to_text())
            .with_context(|| format!("не удалось записать {}", self.paths.env.display()))
    }

    fn set(&mut self, key: &str, value: &str) -> anyhow::Result<()> {
        self.env.set(key, value);
        self.save()
    }

    /// Значение: переменная окружения сильнее файла (как при запуске).
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| self.env.get(key))
    }
}

/// Пройти мастер. `true` — владелец хочет сразу запустить бота.
pub async fn run(paths: &Paths, offer_start: bool) -> anyhow::Result<bool> {
    println!("Настройка exch. Папка проекта: {}", paths.dir.display());
    println!(
        "Секреты сохраняются только в {} — никому его не пересылайте.\n",
        paths.env.display()
    );
    let env = match EnvFile::read(&paths.env)
        .with_context(|| format!("не удалось прочитать {}", paths.env.display()))?
    {
        Some(f) => {
            std::fs::copy(&paths.env, paths.env.with_extension("bak"))
                .context("не удалось сохранить копию .env.bak")?;
            f
        }
        None => EnvFile::parse(ENV_EXAMPLE),
    };
    let mut w = Wizard { paths, env };

    println!("1/6 База данных");
    let pool = database(&mut w).await?;
    println!("\n2/6 Бот");
    let bot = bot(&mut w).await?;
    println!("\n3/6 Владелец");
    owner(&mut w, &bot).await?;
    println!("\n4/6 Telegram API для юзербота");
    telegram_api(&mut w).await?;
    println!("\n5/6 Ключ шифрования сессий");
    session_key(&mut w, &pool).await?;
    config_file(paths)?;
    println!("\n6/6 Юзербот и кошельки");
    userbots(&w, &pool).await?;

    println!("\nГотово. Проверить всё разом: exch check.");
    println!(
        "Запуск бота: дважды щёлкните exch.exe в {} (или наберите там exch).",
        paths.dir.join("target").join("release").display()
    );
    println!(
        "Напишите боту /start — в меню будет «Админ-панель» (её видите только вы и персонал)."
    );
    println!(
        "Направления после установки закрыты: откройте их в пульте, когда пополните кошельки."
    );
    if offer_start {
        return prompt::yes("\nЗапустить бота сейчас?", true).await;
    }
    Ok(false)
}

async fn database(w: &mut Wizard<'_>) -> anyhow::Result<PgPool> {
    if let Some(url) = w.get("DATABASE_URL").filter(|u| !u.contains("<password>")) {
        match db::state(&url).await {
            db::DbState::Unreachable(why) => println!("❌ База из .env: {why}."),
            _ => {
                let pool = db::connect_and_migrate(&url).await?;
                println!("✅ База exch подключена, таблицы на месте.");
                return Ok(pool);
            }
        }
    }
    println!("Нужен пароль пользователя postgres — тот, что вы задали при установке PostgreSQL.");
    loop {
        let password = prompt::hidden("Пароль postgres").await?;
        if password.expose_secret().is_empty() {
            anyhow::bail!(
                "без базы exch не работает: запустите exch setup, когда вспомните пароль"
            );
        }
        let url = db::local_url(password.expose_secret());
        match db::state(&url).await {
            db::DbState::Unreachable(why) => {
                println!("❌ {why}. Попробуйте ещё раз (пустая строка — выйти).");
                println!(
                    "   Если PostgreSQL слушает не порт 5432, поправьте DATABASE_URL в .env вручную."
                );
            }
            state => {
                let pool = db::connect_and_migrate(&url).await?;
                w.set("DATABASE_URL", &url)?;
                if state == db::DbState::Missing {
                    println!("✅ Создана база exch и таблицы.");
                } else {
                    println!("✅ База exch подключена, таблицы на месте.");
                }
                return Ok(pool);
            }
        }
    }
}

/// Ошибка Bot API — понятными словами.
pub fn explain_bot_error(e: &botapi::Error) -> String {
    match e {
        botapi::Error::Api {
            error_code: 401, ..
        }
        | botapi::Error::Api {
            error_code: 404, ..
        } => "Telegram не принял токен — скопируйте его из @BotFather ещё раз".to_owned(),
        botapi::Error::Transport { .. } => {
            "нет связи с api.telegram.org — проверьте интернет".to_owned()
        }
        other => other.to_string(),
    }
}

async fn bot(w: &mut Wizard<'_>) -> anyhow::Result<BotApi> {
    if let Some(token) = w.get("CLIENT_BOT_TOKEN") {
        let api = crate::telegram::bot_api_with(SecretString::from(token), |k| w.get(k))?;
        match api.get_me().await {
            Ok(me) => {
                println!("✅ Бот @{}.", me.username.unwrap_or_default());
                return Ok(api);
            }
            // Нет связи — не повод менять токен.
            Err(botapi::Error::Transport { .. }) => {
                println!(
                    "⚠️ Нет связи с api.telegram.org: токен из .env оставлен без проверки (проверит exch check)."
                );
                return Ok(api);
            }
            Err(e) => println!("❌ Токен из .env: {}.", explain_bot_error(&e)),
        }
    }
    println!("@BotFather → /newbot → он пришлёт токен вида 123456789:AA…");
    println!("Бот один: в нём и клиенты, и ваша админ-панель.");
    loop {
        let token = prompt::hidden("Токен бота").await?;
        let t = token.expose_secret().trim();
        if t.is_empty() {
            anyhow::bail!("без токена бот не запустится: запустите exch setup ещё раз");
        }
        if !looks_like_token(t) {
            println!("❌ Это не похоже на токен: нужен текст вида 123456789:AA… целиком.");
            continue;
        }
        let api = crate::telegram::bot_api_with(SecretString::from(t.to_owned()), |k| w.get(k))?;
        match api.get_me().await {
            Ok(me) => {
                w.set("CLIENT_BOT_TOKEN", t)?;
                println!("✅ Бот @{}.", me.username.unwrap_or_default());
                return Ok(api);
            }
            Err(botapi::Error::Transport { .. })
                if prompt::yes(
                    "⚠️ Нет связи с api.telegram.org. Сохранить токен без проверки?",
                    true,
                )
                .await? =>
            {
                w.set("CLIENT_BOT_TOKEN", t)?;
                return Ok(api);
            }
            Err(e) => println!("❌ {}.", explain_bot_error(&e)),
        }
    }
}

fn looks_like_token(t: &str) -> bool {
    t.split_once(':').is_some_and(|(id, rest)| {
        !id.is_empty()
            && id.bytes().all(|b| b.is_ascii_digit())
            && rest.len() >= 30
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

async fn owner(w: &mut Wizard<'_>, bot: &BotApi) -> anyhow::Result<()> {
    if let Some(id) = w.get("OWNER_TG_ID").and_then(|v| v.parse::<i64>().ok()) {
        println!("✅ Владелец: ID {id}.");
        return Ok(());
    }
    let me = bot
        .get_me()
        .await
        .ok()
        .and_then(|m| m.username)
        .map_or_else(|| "своему боту".to_owned(), |u| format!("боту @{u}"));
    println!(
        "Напишите {me} любое сообщение (например, /start) со своего аккаунта Telegram — так я узнаю ваш ID. Жду до двух минут…"
    );
    if let Some(user) = wait_for_owner(bot).await {
        let name = match &user.username {
            Some(u) => format!("{} (@{u})", user.first_name),
            None => user.first_name.clone(),
        };
        if prompt::yes(&format!("Это вы: {name}, ID {}?", user.id), true).await? {
            w.set("OWNER_TG_ID", &user.id.to_string())?;
            println!("✅ Владелец: ID {}.", user.id);
            return Ok(());
        }
    } else {
        println!("Сообщение не пришло.");
    }
    loop {
        let v = prompt::line("Ваш Telegram ID (число; узнать — у @userinfobot)", None).await?;
        match v.parse::<i64>() {
            Ok(id) if id > 0 => {
                w.set("OWNER_TG_ID", &id.to_string())?;
                println!("✅ Владелец: ID {id}.");
                return Ok(());
            }
            _ => println!("❌ Нужно положительное число, например 123456789."),
        }
    }
}

/// Последний, кто написал боту в личку после начала ожидания. Обновления не подтверждаются
/// (без `offset`): запущенный потом бот получит их как обычно.
async fn wait_for_owner(bot: &BotApi) -> Option<botapi::User> {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
        - 5;
    let deadline = Instant::now() + OWNER_WAIT;
    while Instant::now() < deadline {
        let req = GetUpdates {
            offset: None,
            limit: Some(100),
            timeout: Some(20),
            // `None`: не трогать список типов обновлений бота — Telegram его запоминает.
            allowed_updates: None,
        };
        match bot.get_updates(&req).await {
            Ok(updates) => {
                let found = updates
                    .into_iter()
                    .filter_map(|u| u.message)
                    .filter(|m| m.chat.kind == ChatType::Private && m.date >= since)
                    .filter_map(|m| m.from)
                    .rfind(|u| !u.is_bot);
                if found.is_some() {
                    return found;
                }
            }
            Err(e) => {
                println!(
                    "⚠️ Не получилось прочитать сообщения бота: {}.",
                    explain_bot_error(&e)
                );
                return None;
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    None
}

async fn telegram_api(w: &mut Wizard<'_>) -> anyhow::Result<()> {
    let id_ok = w
        .get("TG_API_ID")
        .is_some_and(|v| v.parse::<i32>().is_ok_and(|n| n > 0));
    let hash_ok = w.get("TG_API_HASH").is_some_and(|v| is_api_hash(&v));
    if id_ok && hash_ok {
        println!("✅ api_id и api_hash заданы.");
        return Ok(());
    }
    println!(
        "my.telegram.org → войдите номером → API development tools → создайте приложение (название любое). Там будут api_id и api_hash."
    );
    if !id_ok {
        loop {
            let v = prompt::line("api_id (число)", None).await?;
            match v.parse::<i32>() {
                Ok(n) if n > 0 => {
                    w.set("TG_API_ID", &n.to_string())?;
                    break;
                }
                _ => println!("❌ api_id — это число, например 1234567."),
            }
        }
    }
    if !hash_ok {
        loop {
            let v = prompt::hidden("api_hash").await?;
            let v = v.expose_secret().trim().to_owned();
            if is_api_hash(&v) {
                w.set("TG_API_HASH", &v)?;
                break;
            }
            println!("❌ api_hash — 32 символа из цифр и букв a–f.");
        }
    }
    println!("✅ api_id и api_hash сохранены.");
    Ok(())
}

fn is_api_hash(v: &str) -> bool {
    v.len() == 32 && v.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Расшифровываются ли этим ключом все сохранённые сессии. `None` — сессий ещё нет.
pub async fn key_opens_sessions(
    pool: &PgPool,
    key: &crate::config::SessionKey,
) -> anyhow::Result<Option<Vec<(String, bool)>>> {
    let mut conn = pool.acquire().await?;
    let key = crate::session_store::session_key(key);
    let mut out = Vec::new();
    for acc in sessions::list(&mut conn).await? {
        if let Some(stored) = sessions::load_session(&mut conn, &acc.label).await? {
            let ok = crate::session_store::from_stored(stored)
                .is_ok_and(|sealed| mtproto::open(&key, &acc.label, &sealed).is_ok());
            out.push((acc.label, ok));
        }
    }
    Ok((!out.is_empty()).then_some(out))
}

async fn session_key(w: &mut Wizard<'_>, pool: &PgPool) -> anyhow::Result<()> {
    if let Some(v) = w.get("SESSION_KEY") {
        match Config::parse_session_key(v) {
            Ok(key) => match key_opens_sessions(pool, &key).await? {
                Some(list) if list.iter().any(|(_, ok)| !ok) => {
                    let bad: Vec<_> = list
                        .iter()
                        .filter(|(_, ok)| !ok)
                        .map(|(l, _)| l.as_str())
                        .collect();
                    println!(
                        "❌ Ключ не подходит к сохранённым сессиям ({}). Если меняли SESSION_KEY — верните прежний в .env.",
                        bad.join(", ")
                    );
                    if !prompt::yes(
                        "Создать новый ключ? Юзерботам придётся войти заново.",
                        false,
                    )
                    .await?
                    {
                        return Ok(());
                    }
                }
                _ => {
                    println!("✅ Ключ задан.");
                    return Ok(());
                }
            },
            Err(_) => println!("❌ SESSION_KEY в .env повреждён."),
        }
    }
    let key = mtproto::SessionKey::random_bytes()
        .map_err(|e| anyhow::anyhow!("не удалось создать ключ: {e}"))?;
    w.set(
        "SESSION_KEY",
        &base64::engine::general_purpose::STANDARD.encode(key),
    )?;
    println!("✅ Создан новый ключ (хранится только в .env; потеряете — юзерботам войти заново).");
    Ok(())
}

fn config_file(paths: &Paths) -> anyhow::Result<()> {
    if paths.config.is_file() {
        if let Err(e) = settings_file::load(&paths.config) {
            println!("⚠️ В config.toml ошибка: {e}");
        }
        return Ok(());
    }
    std::fs::write(&paths.config, settings_file::EXAMPLE)
        .with_context(|| format!("не удалось записать {}", paths.config.display()))?;
    println!(
        "✅ Создан config.toml: часовой пояс, поддержка, логи — меняйте Блокнотом и перезапускайте exch."
    );
    Ok(())
}

async fn userbots(w: &Wizard<'_>, pool: &PgPool) -> anyhow::Result<()> {
    let cfg = Config::from_lookup(|k| w.get(k)).context("настройки после мастера")?;
    let tg = TelegramSettings::from_config(&cfg)?;
    println!(
        "Юзербот — отдельный аккаунт Telegram, на нём кошельки CryptoBot и xRocket с вашим капиталом. Руками им потом не пользуйтесь."
    );
    for platform in Platform::ALL {
        let mut conn = pool.acquire().await?;
        let list = wallets::list(&mut conn).await?;
        let accounts = sessions::list(&mut conn).await?;
        drop(conn);
        let name = platform_name(platform);
        if let Some(wl) = list.iter().find(|w| w.platform == platform && w.is_active) {
            println!("✅ {name}: подключён (аккаунт {}).", wl.userbot_label);
            continue;
        }
        let ask = format!(
            "Подключить кошелёк {name} сейчас? Понадобятся телефон аккаунта, код из Telegram и облачный пароль."
        );
        if !prompt::yes(&ask, true).await? {
            println!("⚠️ {name}: не подключён — запустите exch setup позже.");
            continue;
        }
        // Один аккаунт на оба кошелька: второй вход берёт ту же сессию без кода.
        let label = list
            .first()
            .map(|w| w.userbot_label.clone())
            .or_else(|| accounts.first().map(|a| a.label.clone()))
            .unwrap_or_else(|| "ub-1".to_owned());
        let mut auth = MtprotoAuthenticator {
            cfg: tg.mtproto(&label),
            key: tg.session_key(),
            prompt: mtproto::TerminalPrompt,
        };
        let target = LoginTarget { label, platform };
        match run_login(pool, &target, &mut auth).await {
            Ok(r) => println!(
                "✅ {name}: подключён (аккаунт {}, {}).",
                r.label, r.phone_masked
            ),
            Err(e) => println!("❌ {name}: вход не удался: {e}. Запустите exch setup ещё раз."),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_and_hash_shapes() {
        assert!(looks_like_token(
            "123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw0"
        ));
        assert!(!looks_like_token("CLIENT_BOT_TOKEN=123:abc"));
        assert!(!looks_like_token("123456789"));
        assert!(is_api_hash("0123456789abcdef0123456789ABCDEF"));
        assert!(!is_api_hash("0123456789abcdef"));
    }

    #[test]
    fn env_template_has_every_secret() {
        let f = EnvFile::parse(ENV_EXAMPLE);
        for k in [
            "DATABASE_URL",
            "OWNER_TG_ID",
            "CLIENT_BOT_TOKEN",
            "TG_API_ID",
            "TG_API_HASH",
            "SESSION_KEY",
        ] {
            assert!(f.to_text().contains(&format!("{k}=")), "{k}");
        }
    }
}
