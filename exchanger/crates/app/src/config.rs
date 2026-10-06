//! Конфигурация из переменных окружения (SPEC §12, шаг 8). Секреты — только `SecretString`
//! и `SecretBox`: их `Debug` печатает `[REDACTED]`, в логи они не попадают (CLAUDE.md п. 13).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use secrecy::{SecretBox, SecretString};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("required environment variable {0} is not set")]
    Missing(&'static str),
    #[error("environment variable {var} is invalid: {reason}")]
    Invalid { var: &'static str, reason: String },
}

/// Ключ шифрования сессий юзерботов: 32 байта, в окружении — base64.
pub struct SessionKey(SecretBox<[u8; 32]>);

impl SessionKey {
    pub fn secret(&self) -> &SecretBox<[u8; 32]> {
        &self.0
    }
}

impl std::fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionKey([REDACTED])")
    }
}

/// Настройки процесса. Поля следующих вех — `Option`: `check-config` показывает, чего не хватает.
#[derive(Debug)]
pub struct Config {
    pub database_url: SecretString,
    pub owner_tg_id: i64,
    pub client_bot_token: Option<SecretString>,
    pub ops_bot_token: Option<SecretString>,
    pub cryptopay_token: Option<SecretString>,
    pub xrocket_pay_token: Option<SecretString>,
    pub tg_api_id: Option<i32>,
    pub tg_api_hash: Option<SecretString>,
    pub session_key: Option<SessionKey>,
    pub cryptobot_peer_id: Option<i64>,
    pub xrocket_peer_id: Option<i64>,
    pub healthcheck_url: Option<String>,
    pub tz_owner: String,
}

/// Переменная, которая понадобится в одной из следующих вех.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pending {
    pub var: &'static str,
    pub milestone: &'static str,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Разбор из произвольного источника (тесты). Пустая строка = переменная не задана.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |k: &str| {
            lookup(k)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let secret = |k: &str| get(k).map(SecretString::from);

        let database_url = secret("DATABASE_URL").ok_or(ConfigError::Missing("DATABASE_URL"))?;
        let owner_tg_id = parse_num::<i64>("OWNER_TG_ID", get("OWNER_TG_ID"))?
            .ok_or(ConfigError::Missing("OWNER_TG_ID"))?;
        if owner_tg_id <= 0 {
            return Err(ConfigError::Invalid {
                var: "OWNER_TG_ID",
                reason: "must be a positive Telegram user id".into(),
            });
        }

        Ok(Self {
            database_url,
            owner_tg_id,
            client_bot_token: secret("CLIENT_BOT_TOKEN"),
            ops_bot_token: secret("OPS_BOT_TOKEN"),
            cryptopay_token: secret("CRYPTOPAY_TOKEN"),
            xrocket_pay_token: secret("XROCKET_PAY_TOKEN"),
            tg_api_id: parse_num("TG_API_ID", get("TG_API_ID"))?,
            tg_api_hash: secret("TG_API_HASH"),
            session_key: get("SESSION_KEY").map(parse_session_key).transpose()?,
            cryptobot_peer_id: parse_num("CRYPTOBOT_PEER_ID", get("CRYPTOBOT_PEER_ID"))?,
            xrocket_peer_id: parse_num("XROCKET_PEER_ID", get("XROCKET_PEER_ID"))?,
            healthcheck_url: get("HEALTHCHECK_URL"),
            tz_owner: get("TZ_OWNER").unwrap_or_else(|| "UTC".to_owned()),
        })
    }

    /// Чего не хватает для следующих вех (без значений — только имена переменных).
    pub fn pending(&self) -> Vec<Pending> {
        let checks: [(&'static str, &'static str, bool); 10] = [
            ("CRYPTOPAY_TOKEN", "M2", self.cryptopay_token.is_some()),
            ("XROCKET_PAY_TOKEN", "M2", self.xrocket_pay_token.is_some()),
            ("TG_API_ID", "M3", self.tg_api_id.is_some()),
            ("TG_API_HASH", "M3", self.tg_api_hash.is_some()),
            ("SESSION_KEY", "M3", self.session_key.is_some()),
            ("CRYPTOBOT_PEER_ID", "M3", self.cryptobot_peer_id.is_some()),
            ("XROCKET_PEER_ID", "M3", self.xrocket_peer_id.is_some()),
            ("CLIENT_BOT_TOKEN", "M4", self.client_bot_token.is_some()),
            ("OPS_BOT_TOKEN", "M5", self.ops_bot_token.is_some()),
            ("HEALTHCHECK_URL", "M5", self.healthcheck_url.is_some()),
        ];
        checks
            .into_iter()
            .filter(|(_, _, present)| !present)
            .map(|(var, milestone, _)| Pending { var, milestone })
            .collect()
    }
}

fn parse_num<T: std::str::FromStr>(
    var: &'static str,
    value: Option<String>,
) -> Result<Option<T>, ConfigError> {
    value
        .map(|v| {
            v.parse::<T>().map_err(|_| ConfigError::Invalid {
                var,
                reason: "not a number".into(),
            })
        })
        .transpose()
}

fn parse_session_key(value: String) -> Result<SessionKey, ConfigError> {
    let invalid = |reason: &str| ConfigError::Invalid {
        var: "SESSION_KEY",
        reason: reason.to_owned(),
    };
    let bytes = STANDARD
        .decode(value.as_bytes())
        .map_err(|_| invalid("not valid base64 (generate with: openssl rand -base64 32)"))?;
    let key: [u8; 32] = bytes
        .try_into()
        .map_err(|_| invalid("must decode to exactly 32 bytes"))?;
    Ok(SessionKey(SecretBox::new(Box::new(key))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| map.get(k).cloned()
    }

    const KEY_B64: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="; // 0..31

    #[test]
    fn minimal_config_for_m1() {
        let cfg = Config::from_lookup(env(&[
            ("DATABASE_URL", "postgres://u:hunter2@db/exch"),
            ("OWNER_TG_ID", "42"),
            ("CLIENT_BOT_TOKEN", "  "),
        ]))
        .unwrap();
        assert_eq!(cfg.owner_tg_id, 42);
        assert!(
            cfg.client_bot_token.is_none(),
            "blank values count as unset"
        );
        assert_eq!(cfg.tz_owner, "UTC");
        assert_eq!(cfg.pending().len(), 10);
    }

    #[test]
    fn required_and_invalid_values_are_reported() {
        assert_eq!(
            Config::from_lookup(env(&[("OWNER_TG_ID", "1")])).unwrap_err(),
            ConfigError::Missing("DATABASE_URL")
        );
        assert_eq!(
            Config::from_lookup(env(&[("DATABASE_URL", "x")])).unwrap_err(),
            ConfigError::Missing("OWNER_TG_ID")
        );
        assert!(matches!(
            Config::from_lookup(env(&[("DATABASE_URL", "x"), ("OWNER_TG_ID", "abc")])),
            Err(ConfigError::Invalid {
                var: "OWNER_TG_ID",
                ..
            })
        ));
        assert!(matches!(
            Config::from_lookup(env(&[
                ("DATABASE_URL", "x"),
                ("OWNER_TG_ID", "1"),
                ("SESSION_KEY", "c2hvcnQ=")
            ])),
            Err(ConfigError::Invalid {
                var: "SESSION_KEY",
                ..
            })
        ));
    }

    #[test]
    fn session_key_is_decoded() {
        let cfg = Config::from_lookup(env(&[
            ("DATABASE_URL", "x"),
            ("OWNER_TG_ID", "1"),
            ("SESSION_KEY", KEY_B64),
        ]))
        .unwrap();
        let key = cfg.session_key.unwrap();
        assert_eq!(key.secret().expose_secret()[31], 31);
    }

    /// Debug конфигурации не раскрывает ни одного секрета (CLAUDE.md п. 13).
    #[test]
    fn debug_output_redacts_secrets() {
        let secrets = [
            ("DATABASE_URL", "postgres://u:hunter2@db/exch"),
            ("CLIENT_BOT_TOKEN", "123:client-secret"),
            ("OPS_BOT_TOKEN", "456:ops-secret"),
            ("CRYPTOPAY_TOKEN", "cp-secret"),
            ("XROCKET_PAY_TOKEN", "xr-secret"),
            ("TG_API_HASH", "hash-secret"),
            ("SESSION_KEY", KEY_B64),
        ];
        let mut pairs = secrets.to_vec();
        pairs.push(("OWNER_TG_ID", "1"));
        let cfg = Config::from_lookup(env(&pairs)).unwrap();
        let debug = format!("{cfg:?}");
        for (var, value) in secrets {
            assert!(!debug.contains(value), "{var} leaked in Debug output");
        }
        assert!(!debug.contains("hunter2"));
        assert!(cfg.pending().iter().all(|p| p.var != "CRYPTOPAY_TOKEN"));
    }
}
