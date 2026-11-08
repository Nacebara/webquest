//! `exch check` — проверка по-русски галочками: что настроено, что нет и что сделать. Ничего
//! не меняет и в Telegram юзерботом не входит, поэтому её можно запускать и при работающем
//! боте. Есть ❌ — код выхода ненулевой.

use std::fmt;

use domain::{Direction, Platform};
use secrecy::{ExposeSecret as _, SecretString};
use storage::sessions::UserbotStatus;
use storage::{directions, sessions, wallets};
use ui::EmojiStyle;

use crate::config::{Config, ConfigError};
use crate::db;
use crate::project::Paths;
use crate::settings_file;
use crate::setup::{explain_bot_error, key_opens_sessions, platform_name};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub mark: Mark,
    pub title: String,
    pub detail: String,
}

/// Итог проверки: строки по порядку.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub lines: Vec<Line>,
}

impl Report {
    fn push(&mut self, mark: Mark, title: impl Into<String>, detail: impl Into<String>) {
        self.lines.push(Line {
            mark,
            title: title.into(),
            detail: detail.into(),
        });
    }
    fn ok(&mut self, t: impl Into<String>, d: impl Into<String>) {
        self.push(Mark::Ok, t, d);
    }
    fn warn(&mut self, t: impl Into<String>, d: impl Into<String>) {
        self.push(Mark::Warn, t, d);
    }
    fn fail(&mut self, t: impl Into<String>, d: impl Into<String>) {
        self.push(Mark::Fail, t, d);
    }

    pub fn count(&self, mark: Mark) -> usize {
        self.lines.iter().filter(|l| l.mark == mark).count()
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for l in &self.lines {
            let m = match l.mark {
                Mark::Ok => "✅",
                Mark::Warn => "⚠️",
                Mark::Fail => "❌",
            };
            writeln!(f, "{m} {}: {}", l.title, l.detail)?;
        }
        let (fail, warn) = (self.count(Mark::Fail), self.count(Mark::Warn));
        if fail == 0 && warn == 0 {
            writeln!(f, "\nВсё в порядке.")
        } else {
            writeln!(f, "\nОшибок: {fail}, предупреждений: {warn}.")
        }
    }
}

/// Название направления для людей.
pub fn direction_name(d: Direction) -> &'static str {
    match d {
        Direction::CbToXrCheck => "чек CryptoBot → чек xRocket",
        Direction::XrToCbCheck => "чек xRocket → чек CryptoBot",
        Direction::PayXrInvoice => "оплата счёта xRocket",
        Direction::PayCbInvoice => "оплата счёта CryptoBot",
    }
}

pub async fn run(paths: &Paths) -> Report {
    let mut r = Report::default();
    if paths.env.is_file() {
        r.ok("Файл секретов", paths.env.display().to_string());
    } else if std::env::var_os("DATABASE_URL").is_some() {
        r.ok(
            "Файл секретов",
            "нет .env — настройки из переменных окружения",
        );
    } else {
        r.fail(
            "Файл секретов",
            format!("нет {} — запустите exch setup", paths.env.display()),
        );
    }
    match settings_file::load(&paths.config) {
        Ok(_) if paths.config.is_file() => r.ok("Настройки", paths.config.display().to_string()),
        Ok(_) => r.warn(
            "Настройки",
            "нет config.toml — значения по умолчанию (создаст exch setup)",
        ),
        Err(e) => r.fail("Настройки", format!("ошибка в config.toml: {e}")),
    }
    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(ConfigError::Missing(var)) => {
            r.fail(
                "Настройки",
                format!("не задано {var} — запустите exch setup"),
            );
            return r;
        }
        Err(ConfigError::Invalid { var, reason }) => {
            r.fail("Настройки", format!("{var} неверно: {reason}"));
            return r;
        }
    };
    let db_ok = database(&mut r, cfg.database_url.expose_secret()).await;
    bot(&mut r, &cfg).await;
    if cfg.tg_api_id.is_some() && cfg.tg_api_hash.is_some() {
        r.ok("Telegram API", "api_id и api_hash заданы");
    } else {
        r.fail(
            "Telegram API",
            "нет TG_API_ID или TG_API_HASH — запустите exch setup",
        );
    }
    if !db_ok {
        return r;
    }
    let pool = match storage::connect(cfg.database_url.expose_secret()).await {
        Ok(p) => p,
        Err(e) => {
            r.fail("База данных", db::explain(&e.to_string()));
            return r;
        }
    };
    if let Err(e) = from_database(&mut r, &cfg, &pool).await {
        r.fail("База данных", format!("не удалось прочитать: {e}"));
    }
    r
}

async fn database(r: &mut Report, url: &str) -> bool {
    match db::state(url).await {
        db::DbState::Ready {
            applied,
            pending: 0,
        } => {
            r.ok("База данных", format!("подключено, миграций: {applied}"));
            true
        }
        db::DbState::Ready { applied, pending } => {
            r.warn(
                "База данных",
                format!("подключено, миграций: {applied}; ещё {pending} применятся при запуске"),
            );
            true
        }
        db::DbState::Missing => {
            r.warn("База данных", "базы exch ещё нет — создастся при запуске");
            false
        }
        db::DbState::Unreachable(why) => {
            r.fail(
                "База данных",
                format!("{why} — проверьте DATABASE_URL или exch setup"),
            );
            false
        }
    }
}

async fn bot(r: &mut Report, cfg: &Config) {
    let Some(token) = cfg.client_bot_token.clone() else {
        r.fail("Бот", "нет CLIENT_BOT_TOKEN — запустите exch setup");
        return;
    };
    let api = match botapi::BotApi::new(SecretString::from(token.expose_secret().to_owned())) {
        Ok(a) => a,
        Err(e) => {
            r.fail("Бот", e.to_string());
            return;
        }
    };
    match api.get_me().await {
        Ok(me) => r.ok("Бот", format!("@{}", me.username.unwrap_or_default())),
        Err(e) => {
            r.fail("Бот", explain_bot_error(&e));
            return;
        }
    }
    let owner = serde_json::json!({ "chat_id": cfg.owner_tg_id });
    match api.call::<_, botapi::Chat>("getChat", &owner).await {
        Ok(chat) => r.ok(
            "Владелец",
            format!(
                "ID {} — {}",
                cfg.owner_tg_id,
                chat.first_name.unwrap_or_default()
            ),
        ),
        Err(_) => r.warn(
            "Владелец",
            format!(
                "ID {}: бот вас ещё не видел — напишите ему /start (ID узнать — @userinfobot)",
                cfg.owner_tg_id
            ),
        ),
    }
}

async fn from_database(r: &mut Report, cfg: &Config, pool: &storage::PgPool) -> anyhow::Result<()> {
    match &cfg.session_key {
        None => r.fail("Ключ сессий", "нет SESSION_KEY — запустите exch setup"),
        Some(key) => match key_opens_sessions(pool, key).await? {
            None => r.ok("Ключ сессий", "задан"),
            Some(list) => {
                let bad: Vec<_> = list
                    .iter()
                    .filter(|(_, ok)| !ok)
                    .map(|(l, _)| l.as_str())
                    .collect();
                if bad.is_empty() {
                    r.ok("Ключ сессий", "подходит к сессиям юзерботов");
                } else {
                    r.fail(
                        "Ключ сессий",
                        format!(
                            "не подходит к сессиям {} — верните прежний SESSION_KEY или войдите заново (exch setup)",
                            bad.join(", ")
                        ),
                    );
                }
            }
        },
    }
    let mut conn = pool.acquire().await?;
    let accounts = sessions::list(&mut conn).await?;
    if accounts.is_empty() {
        r.fail("Юзербот", "ни одного аккаунта — запустите exch setup");
    }
    for a in &accounts {
        let title = format!("Юзербот {}", a.label);
        let phone = &a.phone_masked;
        match a.status {
            UserbotStatus::Active if a.has_session => r.ok(title, format!("{phone}, активен")),
            UserbotStatus::Active | UserbotStatus::New => {
                r.fail(title, format!("{phone}: вход не завершён — exch setup"));
            }
            UserbotStatus::FloodWait => r.warn(
                title,
                format!("{phone}: Telegram попросил подождать (FLOOD_WAIT), продолжит сам"),
            ),
            UserbotStatus::Disabled => r.warn(title, format!("{phone}: выключен в пульте")),
            UserbotStatus::LoggedOut => r.fail(
                title,
                format!("{phone}: сессия завершена в Telegram — войдите заново (exch setup)"),
            ),
            UserbotStatus::Banned => {
                r.fail(title, format!("{phone}: аккаунт заблокирован Telegram"))
            }
        }
    }
    let list = wallets::list(&mut conn).await?;
    for p in Platform::ALL {
        let title = format!("Кошелёк {}", platform_name(p));
        match list.iter().find(|w| w.platform == p && w.is_active) {
            Some(w) => r.ok(title, format!("на аккаунте {}", w.userbot_label)),
            None => r.fail(title, "не подключён — запустите exch setup"),
        }
    }
    let dirs = directions::all(&mut conn).await?;
    let open: Vec<_> = dirs
        .iter()
        .filter(|d| d.accepting())
        .map(|d| direction_name(d.direction))
        .collect();
    for d in dirs.iter().filter(|d| d.is_open && d.auto_paused) {
        r.warn(
            "Направление",
            format!(
                "{}: автопауза — {}",
                direction_name(d.direction),
                d.pause_reason.as_deref().unwrap_or("причина в пульте")
            ),
        );
    }
    if open.is_empty() {
        r.warn(
            "Направления",
            "все закрыты — пульт → Направления → карточка → «Открыть приём»",
        );
    } else {
        r.ok("Направления", format!("принимают: {}", open.join(", ")));
    }
    let emoji = crate::emoji::load(&mut conn).await?;
    match emoji.packs.of(emoji.style) {
        None => r.warn("Эмодзи", "выключены — обычные символы"),
        Some(p) if p.is_empty() && emoji.custom.is_empty() => r.warn(
            "Эмодзи",
            "наборы не установлены — пульт → Эмодзи → «Установить»",
        ),
        Some(p) => r.ok(
            "Эмодзи",
            format!(
                "{}: значков {}, своих {}",
                if emoji.style == EmojiStyle::Animated {
                    "анимированные"
                } else {
                    "статичные"
                },
                p.len(),
                emoji.custom.len()
            ),
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_counts_and_prints() {
        let mut r = Report::default();
        r.ok("База данных", "подключено");
        r.fail("Кошелёк xRocket", "не подключён");
        let text = r.to_string();
        assert!(text.contains("✅ База данных: подключено"));
        assert!(text.contains("❌ Кошелёк xRocket: не подключён"));
        assert!(text.contains("Ошибок: 1, предупреждений: 0."));
    }
}
