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

/// Подключить аккаунты всех активных кошельков и собрать для них сценарии.
pub async fn connect_userbots(
    pool: &PgPool,
    tg: &TelegramSettings,
    log: &Arc<WalletLog>,
) -> anyhow::Result<Vec<Userbot>> {
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
    for (label, (userbot_id, platforms)) in accounts {
        let transport = connect_account(pool, tg, &label).await?;
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
    Ok(out)
}

/// Клиентский бот поверх того же движка, который крутит `Engine::run`.
pub fn client_bot(
    pool: &PgPool,
    token: secrecy::SecretString,
    engine: Arc<engine::Engine>,
    support_contact: Option<String>,
) -> anyhow::Result<tgbot::ClientBot> {
    let api = botapi::BotApi::new(token).context("cannot build the Bot API client")?;
    let ui = ui::Ui::builtin(ui::EmojiSet::plain(), ui::UiConfig::default())
        .context("built-in client texts are invalid")?;
    let cfg = tgbot::ClientBotConfig {
        support_contact,
        // Бот показывает пределы заявки так же, как их считает движок при приёме.
        reserve_safety: engine.config().reserve_safety,
        ..tgbot::ClientBotConfig::default()
    };
    Ok(tgbot::ClientBot::new(api, ui, engine, pool.clone(), cfg))
}

/// Движок с кошельками подключённых юзерботов.
pub fn engine(pool: &PgPool, userbots: &[Userbot]) -> engine::Engine {
    let mut engine = engine::Engine::new(pool.clone(), engine::EngineConfig::default());
    for w in userbots.iter().flat_map(|u| u.wallets.iter()) {
        engine = engine.with_wallet(w.clone());
    }
    engine
}
