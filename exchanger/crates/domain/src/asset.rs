//! Активы, платформы и кошельки (SPEC §1.7, §3.1; v0.2 — один личный кошелёк на платформу).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Актив, которым оперирует сервис. Код в БД (`assets.code`) — [`Asset::code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Asset {
    Usdt,
    Ton,
}

impl Asset {
    pub const ALL: [Asset; 2] = [Asset::Usdt, Asset::Ton];

    /// Код в нашей БД и в Crypto Pay.
    pub const fn code(self) -> &'static str {
        match self {
            Asset::Usdt => "USDT",
            Asset::Ton => "TON",
        }
    }

    /// Код актива на конкретной платформе: у xRocket TON называется `TONCOIN`.
    pub const fn platform_code(self, platform: Platform) -> &'static str {
        match (self, platform) {
            (Asset::Ton, Platform::XRocket) => "TONCOIN",
            _ => self.code(),
        }
    }
}

impl fmt::Display for Asset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown asset code: {0}")]
pub struct UnknownAsset(pub String);

impl FromStr for Asset {
    type Err = UnknownAsset;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "USDT" => Ok(Asset::Usdt),
            "TON" | "TONCOIN" => Ok(Asset::Ton),
            other => Err(UnknownAsset(other.to_owned())),
        }
    }
}

/// Платформа-кошелёк.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Platform {
    CryptoBot,
    XRocket,
}

impl Platform {
    pub const ALL: [Platform; 2] = [Platform::CryptoBot, Platform::XRocket];

    /// Значение в БД (`platform` во всех таблицах).
    pub const fn db_name(self) -> &'static str {
        match self {
            Platform::CryptoBot => "cryptobot",
            Platform::XRocket => "xrocket",
        }
    }

    /// Короткий префикс для меток кошельков и счетов леджера.
    pub const fn short(self) -> &'static str {
        match self {
            Platform::CryptoBot => "cb",
            Platform::XRocket => "xr",
        }
    }

    pub fn from_db_name(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.db_name() == s)
    }
}

/// Кошелёк сервиса на платформе — личный баланс аккаунта юзербота (SPEC v0.2: только юзербот,
/// API кошельков не используем). Метка как в `wallet_accounts.label`: `cb`, `xr`.
pub const fn wallet_label(platform: Platform) -> &'static str {
    platform.short()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_codes_round_trip() {
        for a in Asset::ALL {
            assert_eq!(a.code().parse::<Asset>(), Ok(a));
        }
        assert_eq!("TONCOIN".parse::<Asset>(), Ok(Asset::Ton));
        assert!("BTC".parse::<Asset>().is_err());
    }

    #[test]
    fn xrocket_calls_ton_toncoin() {
        assert_eq!(Asset::Ton.platform_code(Platform::XRocket), "TONCOIN");
        assert_eq!(Asset::Ton.platform_code(Platform::CryptoBot), "TON");
        assert_eq!(Asset::Usdt.platform_code(Platform::XRocket), "USDT");
    }

    #[test]
    fn wallet_labels_match_schema() {
        assert_eq!(wallet_label(Platform::CryptoBot), "cb");
        assert_eq!(wallet_label(Platform::XRocket), "xr");
    }

    #[test]
    fn platform_db_names_round_trip() {
        for p in Platform::ALL {
            assert_eq!(Platform::from_db_name(p.db_name()), Some(p));
        }
    }
}
