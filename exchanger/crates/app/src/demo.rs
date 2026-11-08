//! `exch demo` — настоящий бот и пульт на симуляторе кошельков: без денег, без юзербота, на
//! отдельной базе `<база>_demo`. Чтобы пройти экраны клиента и пульта в Telegram и принять их
//! по макетам (PLAN-v1, фаза 1).
//!
//! Что настоящее: клиентский бот, пульт, движок, леджер, сверка, тексты и кнопки. Что
//! симулятор: боты @send и @xrocket (`userbot::sim`, ответы — из живых записей). Чеки и счета
//! для пробы выдаёт команда `/demo` в боте: ссылку копируют и присылают боту как клиент.
//!
//! База демо пересоздаётся при каждом запуске: состояние симулятора живёт в памяти и после
//! перезапуска не совпало бы с леджером. Пересоздаётся только база с именем `…_demo`.
//!
//! Тексты этого модуля — служебные (меню проб), не экраны продукта: экраны берутся из `ui`
//! и `ops` как в бою.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use botapi::{
    AnswerCallbackQuery, BotApi, InlineButtonAction, InlineKeyboardButton, InlineKeyboardMarkup,
    SendMessage,
};
use domain::ledger::{self, AccountCode};
use domain::{Asset, Decimal, Direction, Money, Platform};
use secrecy::SecretString;
use sqlx::migrate::MigrateDatabase as _;
use storage::PgPool;
use storage::ledger::PostingRefs;
use userbot::sim::{AccountId, CheckSpec, InvoiceSpec, SimWorld};
use userbot::{AccountGate, UserbotWallet, WalletBot};

use crate::wallet_log::{AccountIds, WalletLog};

/// Метка аккаунта-юзербота в базе демо.
pub const DEMO_LABEL: &str = "ub-demo";
/// Капитал на каждом кошельке симулятора и в леджере.
const CAPITAL: u32 = 1000;

/// Адрес базы демо: та же строка подключения, имя базы с суффиксом `_demo`.
pub fn demo_database_url(url: &str) -> anyhow::Result<String> {
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (url, None),
    };
    let slash = base
        .rfind('/')
        .filter(|&i| i > base.find("//").map_or(0, |j| j + 1))
        .context("DATABASE_URL has no database name")?;
    let name = &base[slash + 1..];
    anyhow::ensure!(!name.is_empty(), "DATABASE_URL has no database name");
    let demo = if name.ends_with("_demo") {
        name.to_owned()
    } else {
        format!("{name}_demo")
    };
    let mut out = format!("{}/{demo}", &base[..slash]);
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    Ok(out)
}

/// Пересоздать базу демо и применить миграции. Только база `…_demo`.
pub async fn fresh_database(demo_url: &str) -> anyhow::Result<PgPool> {
    let name = demo_url
        .split('?')
        .next()
        .and_then(|b| b.rsplit('/').next())
        .unwrap_or_default();
    anyhow::ensure!(
        name.ends_with("_demo"),
        "refusing to recreate a database that is not a demo one"
    );
    if sqlx::Postgres::database_exists(demo_url)
        .await
        .unwrap_or(false)
    {
        sqlx::Postgres::force_drop_database(demo_url)
            .await
            .context("cannot recreate the demo database")?;
    }
    crate::db::connect_and_migrate(demo_url).await
}

/// Мир симулятора и аккаунт юзербота в нём.
pub struct DemoWorld {
    pub world: SimWorld,
    pub userbot: AccountId,
    /// Аккаунт «клиента» симулятора: открывает чеки для пробы «уже открыт».
    pub stranger: AccountId,
}

/// Наполнить базу демо: аккаунт и кошельки, капитал 1000 USDT на каждом, направления открыты.
pub async fn seed(pool: &PgPool, engine: &engine::Engine) -> anyhow::Result<DemoWorld> {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.as_secs());
    let world = SimWorld::with_seed(seed);
    let userbot = world.add_account("exch_demo");
    let stranger = world.add_account("someone_else");
    world.set_tick(Duration::from_millis(50));

    let mut tx = pool.begin().await?;
    let ub_id = storage::wallets::ensure_userbot(&mut tx, DEMO_LABEL, "+00******00").await?;
    for p in Platform::ALL {
        storage::wallets::ensure_wallet(&mut tx, p, ub_id).await?;
    }
    tx.commit().await?;
    engine.ensure_ledger_accounts().await?;

    let mut tx = pool.begin().await?;
    storage::ledger::ensure_account(
        &mut tx,
        &AccountCode::equity(ledger::Equity::Capital, Asset::Usdt),
        Asset::Usdt,
        None,
    )
    .await?;
    for p in Platform::ALL {
        let amount = Decimal::from(CAPITAL);
        let capital = ledger::capital_in(p, Asset::Usdt, amount)?;
        storage::ledger::post(
            &mut tx,
            &capital,
            PostingRefs {
                memo: Some("demo capital"),
                created_by: Some("demo"),
                ..Default::default()
            },
        )
        .await?;
        world.set_balance(userbot, p, Asset::Usdt, amount)?;
    }
    for d in Direction::ALL {
        storage::directions::set_open(&mut tx, d, true).await?;
    }
    // Как в бою: владелец ставит максимум «лимита без код-пароля» (DESIGN-v0.2 Р11).
    storage::settings::set(
        &mut tx,
        "cb_passcode_free_limit_usd",
        &serde_json::json!(30_000),
        None,
    )
    .await?;
    tx.commit().await?;
    world.set_passcode_free_limit(userbot, Some(Decimal::from(30_000)))?;
    Ok(DemoWorld {
        world,
        userbot,
        stranger,
    })
}

/// Кошельки юзербота на симуляторе для движка (журнал ответов — как в бою).
pub async fn wallets(pool: &PgPool, demo: &DemoWorld) -> anyhow::Result<Vec<Arc<dyn WalletBot>>> {
    let userbot_id = {
        let mut conn = pool.acquire().await?;
        storage::sessions::find(&mut conn, DEMO_LABEL)
            .await?
            .context("demo userbot is missing")?
            .id
    };
    let log = Arc::new(WalletLog::new(pool.clone(), mtproto::PinnedBots::default()));
    log.register(
        DEMO_LABEL,
        AccountIds {
            userbot_id,
            self_id: None,
        },
    );
    let transport = Arc::new(demo.world.transport(demo.userbot));
    let gate = Arc::new(AccountGate::new(DEMO_LABEL));
    Ok(Platform::ALL
        .iter()
        .map(|p| {
            Arc::new(
                UserbotWallet::new(*p, transport.clone(), gate.clone())
                    .with_log(log.clone() as Arc<dyn userbot::MessageLog>),
            ) as Arc<dyn WalletBot>
        })
        .collect())
}

/// Что выдать для пробы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    Check(Platform, u32),
    Invoice(Platform, u32),
    FiatInvoice(Platform, u32),
    Used,
    Multi,
    Password,
    Tiny,
}

impl Item {
    fn parse(data: &str) -> Option<Self> {
        let rest = data.strip_prefix("demo:")?;
        let (kind, amount) = rest.split_once(':').unwrap_or((rest, ""));
        let n = amount.parse::<u32>().ok();
        Some(match (kind, n) {
            ("cb", Some(n)) => Item::Check(Platform::CryptoBot, n),
            ("xr", Some(n)) => Item::Check(Platform::XRocket, n),
            ("icb", Some(n)) => Item::Invoice(Platform::CryptoBot, n),
            ("ixr", Some(n)) => Item::Invoice(Platform::XRocket, n),
            ("fcb", Some(n)) => Item::FiatInvoice(Platform::CryptoBot, n),
            ("used", _) => Item::Used,
            ("multi", _) => Item::Multi,
            ("pass", _) => Item::Password,
            ("tiny", _) => Item::Tiny,
            _ => return None,
        })
    }
}

fn cb(text: &str, data: &str) -> InlineKeyboardButton {
    InlineKeyboardButton::new(text, InlineButtonAction::CallbackData(data.to_owned()))
}

fn menu_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            cb("Чек CryptoBot 10", "demo:cb:10"),
            cb("Чек CryptoBot 100", "demo:cb:100"),
        ],
        vec![
            cb("Чек xRocket 10", "demo:xr:10"),
            cb("Чек xRocket 100", "demo:xr:100"),
        ],
        vec![
            cb("Счёт CryptoBot 50", "demo:icb:50"),
            cb("Счёт xRocket 50", "demo:ixr:50"),
        ],
        vec![
            cb("Счёт CryptoBot $20", "demo:fcb:20"),
            cb("Чек на 600 (сверх лимита)", "demo:cb:600"),
        ],
        vec![
            cb("Чек уже открыт", "demo:used"),
            cb("Мульти-чек xRocket", "demo:multi"),
        ],
        vec![
            cb("Чек с паролем", "demo:pass"),
            cb("Чек на 0.5 (мало)", "demo:tiny"),
        ],
    ])
}

const MENU: &str = "<b>🧪 Демо exch</b>\nКошельки здесь — симулятор: денег нет, остальное как в настоящем боте.\n\nВыберите, что прислать боту: я дам ссылку — скопируйте её и пришлите сюда, как клиент.\nПульт владельца — кнопка «Админ-панель» в меню.";

/// Перехватчик демо: `/demo` и кнопки проб; остальное — пульту и клиентскому боту.
pub struct DemoHook {
    api: BotApi,
    world: SimWorld,
    stranger: AccountId,
    panel: Arc<dyn tgbot::UpdateHook>,
}

impl DemoHook {
    pub fn new(
        token: SecretString,
        demo: &DemoWorld,
        panel: Arc<dyn tgbot::UpdateHook>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            api: crate::telegram::bot_api(token)?,
            world: demo.world.clone(),
            stranger: demo.stranger,
            panel,
        })
    }

    async fn send_menu(&self, chat: i64) {
        let msg = SendMessage::html(chat, MENU).reply_markup(menu_keyboard());
        if let Err(e) = self.api.send_message(&msg).await {
            tracing::warn!(error = %e, "demo: cannot send the menu");
        }
    }

    fn make(&self, item: Item) -> anyhow::Result<(String, String)> {
        let usdt = |n: u32| Money::new(Decimal::from(n), Asset::Usdt);
        let w = &self.world;
        Ok(match item {
            Item::Check(p, n) => (
                format!("Чек {} · {n} USDT", crate::setup::platform_name(p)),
                w.client_check(p, usdt(n)?)?,
            ),
            Item::Invoice(p, n) => (
                format!("Счёт {} · {n} USDT", crate::setup::platform_name(p)),
                w.create_invoice(InvoiceSpec::crypto(p, usdt(n)?))?.url,
            ),
            Item::FiatInvoice(p, n) => (
                format!(
                    "Счёт {} · ${n} (в долларах)",
                    crate::setup::platform_name(p)
                ),
                w.create_invoice(InvoiceSpec::fiat(
                    p,
                    Decimal::from(n),
                    "USD",
                    &[Asset::Usdt],
                ))?
                .url,
            ),
            Item::Used => {
                let url = w.client_check(Platform::CryptoBot, usdt(10)?)?;
                w.claim_check(self.stranger, &url)?;
                (
                    "Чек CryptoBot · 10 USDT, его уже открыл другой".to_owned(),
                    url,
                )
            }
            Item::Multi => (
                "Мульти-чек xRocket · 3 × 10 USDT".to_owned(),
                w.create_check(CheckSpec::new(Platform::XRocket, usdt(10)?).multi(3))?
                    .url,
            ),
            Item::Password => (
                "Чек CryptoBot с паролем · 10 USDT".to_owned(),
                w.create_check(
                    CheckSpec::new(Platform::CryptoBot, usdt(10)?).with_password("1234"),
                )?
                .url,
            ),
            Item::Tiny => (
                "Чек CryptoBot · 0.5 USDT".to_owned(),
                w.client_check(
                    Platform::CryptoBot,
                    Money::new(Decimal::new(5, 1), Asset::Usdt)?,
                )?,
            ),
        })
    }

    async fn give(&self, chat: i64, item: Item) {
        let text = match self.make(item) {
            Ok((title, url)) => {
                let msg = SendMessage::html(
                    chat,
                    format!(
                        "<b>{}</b>\n<code>{}</code>\nСкопируйте ссылку и пришлите её сюда.",
                        ui::html::esc(&title),
                        ui::html::esc(&url)
                    ),
                )
                .reply_markup(InlineKeyboardMarkup::new(vec![
                    vec![InlineKeyboardButton::new(
                        "Скопировать ссылку",
                        InlineButtonAction::CopyText(url),
                    )],
                    vec![cb("Ещё пробы", "demo:menu")],
                ]));
                if let Err(e) = self.api.send_message(&msg).await {
                    tracing::warn!(error = %e, "demo: cannot send the link");
                }
                return;
            }
            Err(e) => format!("Симулятор не смог: {}", ui::html::esc(&e.to_string())),
        };
        let _ = self.api.send_message(&SendMessage::html(chat, text)).await;
    }
}

#[async_trait::async_trait]
impl tgbot::UpdateHook for DemoHook {
    async fn try_handle(&self, update: &botapi::Update) -> bool {
        if let Some(m) = &update.message {
            let text = m.text.as_deref().unwrap_or_default().trim();
            if text == "/demo" {
                self.send_menu(m.chat.id).await;
                return true;
            }
            if text == "/start" {
                // Приветствие покажет клиентский бот, меню проб — следом за ним.
                let api = self.api.clone();
                let chat = m.chat.id;
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    let msg = SendMessage::html(chat, MENU).reply_markup(menu_keyboard());
                    let _ = api.send_message(&msg).await;
                });
            }
        }
        if let Some(q) = &update.callback_query
            && let Some(data) = q.data.as_deref()
            && data.starts_with("demo:")
        {
            let _ = self
                .api
                .answer_callback_query(&AnswerCallbackQuery::ack(q.id.clone()))
                .await;
            let chat = q.message.as_ref().map_or(q.from.id, |m| m.chat.id);
            match Item::parse(data) {
                Some(item) => self.give(chat, item).await,
                None => self.send_menu(chat).await,
            }
            return true;
        }
        self.panel.try_handle(update).await
    }

    async fn menu_button(&self, user: i64) -> Option<String> {
        self.panel.menu_button(user).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_database_gets_its_own_name() {
        assert_eq!(
            demo_database_url("postgres://postgres:p%40ss@localhost:5432/exch").unwrap(),
            "postgres://postgres:p%40ss@localhost:5432/exch_demo"
        );
        assert_eq!(
            demo_database_url("postgres://u@h/exch?sslmode=disable").unwrap(),
            "postgres://u@h/exch_demo?sslmode=disable"
        );
        assert_eq!(
            demo_database_url("postgres://u@h/exch_demo").unwrap(),
            "postgres://u@h/exch_demo"
        );
        assert!(demo_database_url("postgres://u@h").is_err());
    }

    #[test]
    fn probe_buttons_parse() {
        assert_eq!(
            Item::parse("demo:cb:100"),
            Some(Item::Check(Platform::CryptoBot, 100))
        );
        assert_eq!(Item::parse("demo:used"), Some(Item::Used));
        assert_eq!(Item::parse("demo:menu"), None);
        for row in menu_keyboard().inline_keyboard {
            for b in row {
                if let InlineButtonAction::CallbackData(d) = b.action {
                    assert!(d.len() <= 64);
                    assert!(Item::parse(&d).is_some(), "{d}");
                }
            }
        }
    }
}
