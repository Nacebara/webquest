//! Сборка рабочего процесса `exch run`: юзерботы активных кошельков → движок.
//!
//! Один аккаунт Telegram — одно соединение MTProto и одни ворота ([`AccountGate`]), даже если
//! аккаунт обслуживает кошельки обеих платформ: одно действие в полёте на аккаунт (CLAUDE.md
//! п. 12). Каждое сообщение ботов кошельков уходит в журнал `wallet_messages` до разбора.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Context as _;
use domain::Platform;
use mtproto::GrammersTransport;
use storage::PgPool;
use userbot::{AccountGate, UserbotWallet, WalletBot};

use crate::session_store::PgSessionStore;
use crate::telegram::TelegramSettings;
use crate::wallet_log::{AccountIds, WalletLog};

/// Подключённый юзербот: аккаунт и его кошельки.
pub struct Userbot {
    pub label: String,
    pub transport: Arc<GrammersTransport>,
    pub wallets: Vec<Arc<dyn WalletBot>>,
}

impl std::fmt::Debug for Userbot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Userbot")
            .field("label", &self.label)
            .field(
                "platforms",
                &self
                    .wallets
                    .iter()
                    .map(|w| w.platform())
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

/// Подключить аккаунт `label` по сохранённой сессии.
pub async fn connect_account(
    pool: &PgPool,
    tg: &TelegramSettings,
    label: &str,
) -> anyhow::Result<Arc<GrammersTransport>> {
    let store = Arc::new(PgSessionStore::new(pool.clone(), label));
    let transport = GrammersTransport::connect(tg.mtproto(label), tg.session_key(), store)
        .await
        .with_context(|| {
            format!("cannot connect userbot {label} (run `exch login --label {label}`)")
        })?;
    Ok(Arc::new(transport))
}

/// Подключить аккаунты всех активных кошельков и собрать для них сценарии. Любой аккаунт не
/// вошёл — ошибка (самопроверка и служебные команды).
pub async fn connect_userbots(
    pool: &PgPool,
    tg: &TelegramSettings,
    log: &Arc<WalletLog>,
) -> anyhow::Result<Vec<Userbot>> {
    Ok(connect(pool, tg, log, false).await?.userbots)
}

/// Итог подключения для работы (`exch run`).
#[derive(Debug)]
pub struct Connected {
    pub userbots: Vec<Userbot>,
    /// Аккаунты, которые не вошли (вход сброшен): работаем без них.
    pub lost: Vec<String>,
    /// Сколько аккаунтов у активных кошельков всего.
    pub accounts: usize,
}

/// Для `exch run` (Э-19.1, проверка 19.11): аккаунт, у которого нет сессии или Telegram её
/// отозвал, не останавливает exch — он пропущен, его направления на паузе, персоналу тревога
/// «Вход сброшен». Клиентский бот, пульт и другой аккаунт работают.
pub async fn connect_userbots_for_run(
    pool: &PgPool,
    tg: &TelegramSettings,
    log: &Arc<WalletLog>,
) -> anyhow::Result<Connected> {
    connect(pool, tg, log, true).await
}

/// Вход сброшен: сессии нет, Telegram её отозвал или её не расшифровать этим ключом.
pub fn login_lost(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<mtproto::ConnectError>().is_some_and(|c| {
            matches!(
                c,
                mtproto::ConnectError::NoSession
                    | mtproto::ConnectError::NotAuthorized
                    | mtproto::ConnectError::Session(_)
            )
        })
    })
}

async fn connect(
    pool: &PgPool,
    tg: &TelegramSettings,
    log: &Arc<WalletLog>,
    skip_lost: bool,
) -> anyhow::Result<Connected> {
    let rows = {
        let mut conn = pool.acquire().await?;
        storage::wallets::list(&mut conn).await?
    };
    // Метка аккаунта → (id аккаунта, платформы его активных кошельков).
    let mut accounts: BTreeMap<String, (i16, Vec<Platform>)> = BTreeMap::new();
    for row in rows.into_iter().filter(|r| r.is_active) {
        accounts
            .entry(row.userbot_label)
            .or_insert_with(|| (row.userbot_id, Vec::new()))
            .1
            .push(row.platform);
    }
    let mut out = Vec::new();
    let mut lost = Vec::new();
    let total = accounts.len();
    for (label, (userbot_id, platforms)) in accounts {
        // После перезагрузки сеть поднимается не сразу: ждать её, но не «вход сброшен» (шаг 2.3).
        let connected = crate::startup::retry(
            "Telegram",
            crate::startup::WAIT_LIMIT,
            || connect_account(pool, tg, &label),
            crate::startup::transient_telegram,
        )
        .await;
        let transport = match connected {
            Ok(t) => t,
            Err(e) if skip_lost && login_lost(&e) => {
                tracing::error!(account = %label, error = %e, "userbot is not logged in: exch runs without it");
                let mut conn = pool.acquire().await?;
                engine::account_alerts::account_not_connected(
                    &mut conn, &label, userbot_id, &platforms,
                )
                .await?;
                lost.push(label);
                continue;
            }
            Err(e) => return Err(e),
        };
        if skip_lost {
            let mut conn = pool.acquire().await?;
            engine::account_alerts::account_connected(&mut conn, &platforms).await?;
        }
        log.register(
            &label,
            AccountIds {
                userbot_id,
                self_id: Some(transport.self_id()),
            },
        );
        let gate = Arc::new(AccountGate::new(label.clone()));
        let wallets = platforms
            .iter()
            .map(|p| {
                Arc::new(
                    UserbotWallet::new(*p, transport.clone(), gate.clone())
                        .with_log(log.clone() as Arc<dyn userbot::MessageLog>),
                ) as Arc<dyn WalletBot>
            })
            .collect();
        tracing::info!(account = %label, ?platforms, "userbot connected");
        out.push(Userbot {
            label,
            transport,
            wallets,
        });
    }
    Ok(Connected {
        userbots: out,
        lost,
        accounts: total,
    })
}

/// Клиентский бот поверх того же движка, который крутит `Engine::run`.
pub fn client_bot(
    pool: &PgPool,
    token: secrecy::SecretString,
    engine: Arc<engine::Engine>,
    support_contact: Option<String>,
    emoji: ui::EmojiSet,
) -> anyhow::Result<tgbot::ClientBot> {
    let api = crate::telegram::bot_api(token)?;
    let ui = ui::Ui::builtin(emoji, ui::UiConfig::default())
        .context("built-in client texts are invalid")?;
    let cfg = tgbot::ClientBotConfig {
        support_contact,
        // Бот показывает пределы заявки так же, как их считает движок при приёме.
        reserve_safety: engine.config().reserve_safety,
        ..tgbot::ClientBotConfig::default()
    };
    Ok(tgbot::ClientBot::new(api, ui, engine, pool.clone(), cfg))
}

/// Админ-панель в клиентском боте (тот же токен). Владелец из `OWNER_TG_ID` создаётся при
/// старте, если его ещё нет.
pub fn ops_bot(
    pool: &PgPool,
    token: secrecy::SecretString,
    engine: Arc<engine::Engine>,
    owner_tg_id: i64,
) -> anyhow::Result<ops::OpsBot> {
    let api = crate::telegram::bot_api(token)?;
    let cfg = ops::OpsConfig {
        owner_id: Some(owner_tg_id),
        tz: std::env::var("TZ_OWNER")
            .ok()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| "Europe/Moscow".to_owned()),
        ..ops::OpsConfig::default()
    };
    ops::OpsBot::new(api, engine, pool.clone(), cfg).context("built-in ops texts are invalid")
}

/// Эмодзи клиентского бота из настроек (`exch emoji`). Вариация «off» или битые настройки —
/// обычные символы: интерфейс от этого не ломается, ошибка — в лог.
pub async fn emoji_set(pool: &PgPool) -> ui::EmojiSet {
    let loaded = async {
        let mut conn = pool.acquire().await?;
        let settings = crate::emoji::load(&mut conn).await?;
        let premium = settings.style != ui::EmojiStyle::Off;
        anyhow::Ok(ui::EmojiSet::from_settings(premium, &settings)?)
    }
    .await;
    match loaded {
        Ok(set) => set,
        Err(e) => {
            tracing::error!(error = %e, "emoji settings are invalid, plain emoji are used");
            ui::EmojiSet::plain()
        }
    }
}

/// Движок с кошельками подключённых юзерботов.
pub fn engine(pool: &PgPool, userbots: &[Userbot]) -> engine::Engine {
    let mut engine = engine::Engine::new(pool.clone(), engine::EngineConfig::default());
    for w in userbots.iter().flat_map(|u| u.wallets.iter()) {
        engine = engine.with_wallet(w.clone());
    }
    // Чужие входы в аккаунты юзерботов — тревога (веха 10).
    for u in userbots {
        engine = engine.with_account(&u.label, u.transport.clone());
    }
    engine
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Э-19.1: «вход сброшен» — пропустить аккаунт; сеть, таймаут и сверка ботов — нет.
    #[test]
    fn only_a_lost_login_lets_exch_run_without_the_account() {
        let lost = |e: mtproto::ConnectError| login_lost(&anyhow::Error::new(e).context("connect"));
        assert!(lost(mtproto::ConnectError::NoSession));
        assert!(lost(mtproto::ConnectError::NotAuthorized));
        assert!(!lost(mtproto::ConnectError::Timeout("connect")));
        assert!(!lost(mtproto::ConnectError::InvalidConfig("api_id".into())));
    }
}
