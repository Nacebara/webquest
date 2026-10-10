//! Настройки подключения аккаунта юзербота: api_id/api_hash, закреплённые боты кошельков,
//! параметры устройства и таймауты.

use std::time::Duration;

use domain::Platform;
use secrecy::SecretString;

/// Бот кошелька, закреплённый по user id (CLAUDE.md п. 10): username из ссылок клиента
/// никогда не резолвится, а username из конфига используется только для сверки при старте.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedBot {
    pub platform: Platform,
    /// User id бота (MTProto, без префиксов Bot API).
    pub id: i64,
    /// Ожидаемый username без `@`; сравнивается без учёта регистра с основным и активными
    /// дополнительными username бота.
    pub username: String,
}

/// CryptoBot: `@send` (алиас `@CryptoBot`), id из lovec (`tests/send_outcomes.rs`, `claimer.rs:431`).
pub const CRYPTOBOT_ID: i64 = 1_559_501_630;
pub const CRYPTOBOT_USERNAME: &str = "send";
/// xRocket: `@xrocket` (алиас `@tonRocketBot`).
pub const XROCKET_ID: i64 = 5_014_831_088;
pub const XROCKET_USERNAME: &str = "xrocket";

impl PinnedBot {
    /// Закреплённый бот платформы по умолчанию.
    pub fn default_for(platform: Platform) -> Self {
        match platform {
            Platform::CryptoBot => Self {
                platform,
                id: CRYPTOBOT_ID,
                username: CRYPTOBOT_USERNAME.to_owned(),
            },
            Platform::XRocket => Self {
                platform,
                id: XROCKET_ID,
                username: XROCKET_USERNAME.to_owned(),
            },
        }
    }
}

/// Оба бота кошельков.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedBots {
    pub cryptobot: PinnedBot,
    pub xrocket: PinnedBot,
}

impl Default for PinnedBots {
    fn default() -> Self {
        Self {
            cryptobot: PinnedBot::default_for(Platform::CryptoBot),
            xrocket: PinnedBot::default_for(Platform::XRocket),
        }
    }
}

impl PinnedBots {
    pub fn get(&self, platform: Platform) -> &PinnedBot {
        match platform {
            Platform::CryptoBot => &self.cryptobot,
            Platform::XRocket => &self.xrocket,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &PinnedBot> {
        [&self.cryptobot, &self.xrocket].into_iter()
    }

    /// Платформа по user id бота, если это один из закреплённых.
    pub fn platform_of(&self, user_id: i64) -> Option<Platform> {
        self.iter().find(|b| b.id == user_id).map(|b| b.platform)
    }

    /// Проверка конфигурации: id положительные и разные, платформы на своих местах.
    pub fn validate(&self) -> Result<(), String> {
        if self.cryptobot.platform != Platform::CryptoBot
            || self.xrocket.platform != Platform::XRocket
        {
            return Err("pinned bots are assigned to the wrong platforms".into());
        }
        if self
            .iter()
            .any(|b| b.id <= 0 || b.username.trim().is_empty())
        {
            return Err("pinned bot id must be positive and username non-empty".into());
        }
        if self.cryptobot.id == self.xrocket.id {
            return Err("CryptoBot and xRocket must have different pinned ids".into());
        }
        Ok(())
    }
}

/// Что сервер видит в `initConnection`. Значения постоянные и правдоподобные (бан-гигиена);
/// язык закреплён `ru`, чтобы ответы ботов совпадали с фикстурами парсеров (LOVEC-PORTING §5.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub device_model: String,
    pub system_version: String,
    pub app_version: String,
    pub lang_code: String,
    pub system_lang_code: String,
}

impl Default for DeviceInfo {
    fn default() -> Self {
        Self {
            device_model: "PC 64bit".to_owned(),
            system_version: "Linux".to_owned(),
            app_version: concat!("exch ", env!("CARGO_PKG_VERSION")).to_owned(),
            lang_code: "ru".to_owned(),
            system_lang_code: "ru".to_owned(),
        }
    }
}

/// Таймауты и интервалы. Каждый RPC обёрнут в `tokio::time::timeout`: в grammers 0.10 нет
/// таймаута подключения и pong-таймаута (LOVEC-PORTING §5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timeouts {
    /// Денежные и прочие действия (`sendMessage`, нажатие, инлайн, web view).
    pub action: Duration,
    /// Чтение (`getHistory`, `getMessages`, `getUsers`).
    pub read: Duration,
    /// Первое подключение и проверки при старте (`get_me`, resolve ботов).
    pub startup: Duration,
    /// Сколько ждать восстановления соединения, прежде чем отказать в запросе (запрос не уходит).
    pub reconnect_wait: Duration,
    /// Период пинга сторожа.
    pub ping_every: Duration,
    /// Таймаут одного пинга.
    pub ping_timeout: Duration,
    /// После скольких подряд неудачных пингов пересоздать пул соединений.
    pub ping_failures_before_reconnect: u32,
    /// Как часто сохранять состояние апдейтов (pts/qts) в сессию.
    pub update_state_sync_every: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            action: Duration::from_secs(10),
            read: Duration::from_secs(15),
            startup: Duration::from_secs(30),
            reconnect_wait: Duration::from_secs(10),
            ping_every: Duration::from_secs(10),
            ping_timeout: Duration::from_secs(5),
            ping_failures_before_reconnect: 2,
            update_state_sync_every: Duration::from_secs(30),
        }
    }
}

/// Всё, что нужно для подключения одного аккаунта.
#[derive(Debug, Clone)]
pub struct MtprotoConfig {
    /// Метка аккаунта (`userbot_accounts.label`, например `ub-cryptobot-1`): входит в AAD
    /// шифрования сессии и в спаны логов.
    pub account: String,
    pub api_id: i32,
    /// Нужен только при входе (`auth.sendCode`).
    pub api_hash: SecretString,
    pub bots: PinnedBots,
    pub device: DeviceInfo,
    pub timeouts: Timeouts,
    /// `platform` для `messages.requestWebView` (`android`, `ios`, `tdesktop`, `web`).
    /// Влияние на мини-приложение CryptoBot не проверено (LOVEC-PORTING §8.3, P4).
    pub webapp_platform: String,
}

impl MtprotoConfig {
    pub fn new(account: impl Into<String>, api_id: i32, api_hash: SecretString) -> Self {
        Self {
            account: account.into(),
            api_id,
            api_hash,
            bots: PinnedBots::default(),
            device: DeviceInfo::default(),
            timeouts: Timeouts::default(),
            webapp_platform: "tdesktop".to_owned(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.account.trim().is_empty() {
            return Err("account label must not be empty".into());
        }
        if self.api_id <= 0 {
            return Err("api_id must be positive".into());
        }
        self.bots.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[test]
    fn default_pins_match_lovec_ids() {
        let bots = PinnedBots::default();
        assert_eq!(bots.cryptobot.id, 1_559_501_630);
        assert_eq!(bots.cryptobot.username, "send");
        assert_eq!(bots.xrocket.id, 5_014_831_088);
        assert_eq!(bots.xrocket.username, "xrocket");
        assert_eq!(bots.platform_of(1_559_501_630), Some(Platform::CryptoBot));
        assert_eq!(bots.platform_of(5_014_831_088), Some(Platform::XRocket));
        assert_eq!(bots.platform_of(42), None);
        assert!(bots.validate().is_ok());
    }

    #[test]
    fn invalid_pins_are_rejected() {
        let mut bots = PinnedBots::default();
        bots.xrocket.id = bots.cryptobot.id;
        assert!(bots.validate().is_err());
        let mut bots = PinnedBots::default();
        std::mem::swap(&mut bots.cryptobot, &mut bots.xrocket);
        assert!(bots.validate().is_err());
        let mut bots = PinnedBots::default();
        bots.cryptobot.username = " ".into();
        assert!(bots.validate().is_err());
    }

    #[test]
    fn config_debug_hides_api_hash() {
        let cfg = MtprotoConfig::new("ub-1", 12345, SecretString::from("deadbeefcafe"));
        assert!(cfg.validate().is_ok());
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("deadbeefcafe"), "{dbg}");
        assert_eq!(cfg.api_hash.expose_secret(), "deadbeefcafe");
        assert_eq!(cfg.device.lang_code, "ru");
    }
}
