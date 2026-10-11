//! Настройки подключения аккаунтов юзерботов из конфигурации: `TG_API_ID`, `TG_API_HASH`,
//! `SESSION_KEY` и закреплённые боты кошельков (`mtproto::config`). Общие для `exch login`,
//! `exch record`, `exch run` и `exch check-config`.

use mtproto::config::{CRYPTOBOT_ID, XROCKET_ID};
use mtproto::{MtprotoConfig, PinnedBots};
use secrecy::{SecretBox, SecretString};

use crate::config::Config;
use crate::session_store::{SESSION_KEY_VERSION, copy_key};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TelegramConfigError {
    /// Не заданы переменные (только имена, без значений).
    #[error("userbot settings are incomplete, set: {}", .0.join(", "))]
    Missing(Vec<&'static str>),
    /// `*_PEER_ID` из окружения не совпал с закреплённым id: значит, кто-то ждёт другого бота.
    #[error(
        "{var} = {configured} differs from the pinned bot id {pinned}: wallet bots are pinned in mtproto::config; remove {var} or fix it"
    )]
    PinMismatch {
        var: &'static str,
        configured: i64,
        pinned: i64,
    },
}

/// Закреплённые боты. Переменные `CRYPTOBOT_PEER_ID` / `XROCKET_PEER_ID` — только сверка:
/// писать другому боту по настройке окружения нельзя (CLAUDE.md п. 11).
pub fn pinned_bots(cfg: &Config) -> Result<PinnedBots, TelegramConfigError> {
    for (var, configured, pinned) in [
        ("CRYPTOBOT_PEER_ID", cfg.cryptobot_peer_id, CRYPTOBOT_ID),
        ("XROCKET_PEER_ID", cfg.xrocket_peer_id, XROCKET_ID),
    ] {
        if let Some(configured) = configured
            && configured != pinned
        {
            return Err(TelegramConfigError::PinMismatch {
                var,
                configured,
                pinned,
            });
        }
    }
    Ok(PinnedBots::default())
}

/// Чего не хватает в окружении для работы юзерботов.
pub fn missing_vars(cfg: &Config) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if cfg.tg_api_id.is_none() {
        missing.push("TG_API_ID");
    }
    if cfg.tg_api_hash.is_none() {
        missing.push("TG_API_HASH");
    }
    if cfg.session_key.is_none() {
        missing.push("SESSION_KEY");
    }
    missing
}

/// Всё, что нужно для подключения любого аккаунта. `Debug` не раскрывает секретов.
#[derive(Debug)]
pub struct TelegramSettings {
    api_id: i32,
    api_hash: SecretString,
    session_key: SecretBox<[u8; 32]>,
    bots: PinnedBots,
}

impl TelegramSettings {
    pub fn from_config(cfg: &Config) -> Result<Self, TelegramConfigError> {
        let bots = pinned_bots(cfg)?;
        match (&cfg.tg_api_id, &cfg.tg_api_hash, &cfg.session_key) {
            (Some(api_id), Some(api_hash), Some(key)) => Ok(Self {
                api_id: *api_id,
                api_hash: api_hash.clone(),
                session_key: copy_key(key.secret()),
                bots,
            }),
            _ => Err(TelegramConfigError::Missing(missing_vars(cfg))),
        }
    }

    /// Для тестов: настройки без переменных окружения.
    pub fn new(
        api_id: i32,
        api_hash: SecretString,
        session_key: SecretBox<[u8; 32]>,
        bots: PinnedBots,
    ) -> Self {
        Self {
            api_id,
            api_hash,
            session_key,
            bots,
        }
    }

    /// Настройки MTProto для аккаунта `label` (метка — часть AAD шифра сессии).
    pub fn mtproto(&self, label: &str) -> MtprotoConfig {
        let mut cfg = MtprotoConfig::new(label, self.api_id, self.api_hash.clone());
        cfg.bots = self.bots.clone();
        cfg
    }

    /// Новый экземпляр ключа сессий (`GrammersTransport::connect` забирает ключ себе).
    pub fn session_key(&self) -> mtproto::SessionKey {
        mtproto::SessionKey::new(SESSION_KEY_VERSION, copy_key(&self.session_key))
    }

    pub fn bots(&self) -> &PinnedBots {
        &self.bots
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(pairs: &[(&str, &str)]) -> Config {
        let mut map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        map.insert("DATABASE_URL".into(), "postgres://u:p@db/exch".into());
        map.insert("OWNER_TG_ID".into(), "1".into());
        Config::from_lookup(move |k| map.get(k).cloned()).unwrap()
    }

    const KEY_B64: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

    #[test]
    fn missing_variables_are_named_without_values() {
        let err = TelegramSettings::from_config(&config(&[("TG_API_HASH", "hash-secret")]))
            .unwrap_err();
        assert_eq!(
            err,
            TelegramConfigError::Missing(vec!["TG_API_ID", "SESSION_KEY"])
        );
        assert!(!err.to_string().contains("hash-secret"));
    }

    #[test]
    fn complete_settings_build_mtproto_config_and_key() {
        let cfg = config(&[
            ("TG_API_ID", "12345"),
            ("TG_API_HASH", "hash-secret"),
            ("SESSION_KEY", KEY_B64),
            ("CRYPTOBOT_PEER_ID", "1559501630"),
        ]);
        let tg = TelegramSettings::from_config(&cfg).unwrap();
        let m = tg.mtproto("ub-1");
        assert_eq!(m.account, "ub-1");
        assert_eq!(m.api_id, 12345);
        assert_eq!(m.bots, PinnedBots::default());
        assert_eq!(tg.session_key().version(), SESSION_KEY_VERSION);
        // Ключ тот же, что в конфигурации: зашифрованное одним открывается другим.
        let sealed = mtproto::seal(&tg.session_key(), "ub-1", b"x").unwrap();
        let cfg_key = crate::session_store::session_key(cfg.session_key.as_ref().unwrap());
        assert_eq!(
            mtproto::open(&cfg_key, "ub-1", &sealed).unwrap().as_slice(),
            b"x"
        );
        let dbg = format!("{tg:?}");
        assert!(!dbg.contains("hash-secret"), "{dbg}");
    }

    #[test]
    fn peer_id_variables_only_confirm_the_pins() {
        let err = pinned_bots(&config(&[("XROCKET_PEER_ID", "42")])).unwrap_err();
        assert_eq!(
            err,
            TelegramConfigError::PinMismatch {
                var: "XROCKET_PEER_ID",
                configured: 42,
                pinned: XROCKET_ID,
            }
        );
        assert_eq!(
            pinned_bots(&config(&[("XROCKET_PEER_ID", "5014831088")])).unwrap(),
            PinnedBots::default()
        );
    }
}
