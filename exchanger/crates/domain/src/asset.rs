//! Активы, платформы и кошельки (SPEC §1.7, §3.1).

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

/// Личный баланс аккаунта (юзербот) или баланс приложения (API).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum WalletKind {
    Personal,
    App,
}

impl WalletKind {
    pub const fn db_name(self) -> &'static str {
        match self {
            WalletKind::Personal => "personal",
            WalletKind::App => "app",
        }
    }
}

/// Один из четырёх кошельков (SPEC §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WalletRef {
    pub platform: Platform,
    pub kind: WalletKind,
}

impl WalletRef {
    pub const fn new(platform: Platform, kind: WalletKind) -> Self {
        Self { platform, kind }
    }

    pub const ALL: [WalletRef; 4] = [
        WalletRef::new(Platform::CryptoBot, WalletKind::Personal),
        WalletRef::new(Platform::CryptoBot, WalletKind::App),
        WalletRef::new(Platform::XRocket, WalletKind::Personal),
        WalletRef::new(Platform::XRocket, WalletKind::App),
    ];

    /// Метка как в `wallet_accounts.label`: `cb:personal`, `xr:app`.
    pub fn label(self) -> String {
        format!("{}:{}", self.platform.short(), self.kind.db_name())
    }
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
        let labels: Vec<String> = WalletRef::ALL.iter().map(|w| w.label()).collect();
        assert_eq!(labels, ["cb:personal", "cb:app", "xr:personal", "xr:app"]);
    }

    #[test]
    fn platform_db_names_round_trip() {
        for p in Platform::ALL {
            assert_eq!(Platform::from_db_name(p.db_name()), Some(p));
        }
    }
}
