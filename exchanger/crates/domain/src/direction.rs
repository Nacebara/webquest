//! Направления обмена (`directions.code`, SPEC §2.3).
//! Основной спрос — CryptoBot → xRocket: люди уходят из CryptoBot и забирают баланс в xRocket.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::asset::Platform;
use crate::fsm::Flow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Direction {
    /// Чек xRocket → чек CryptoBot (встречный поток, пополняет сторону xRocket).
    XrToCbCheck,
    /// Чек CryptoBot → чек xRocket — основной спрос.
    CbToXrCheck,
    /// Оплата счёта CryptoBot деньгами клиента из xRocket.
    PayCbInvoice,
    /// Оплата счёта xRocket деньгами клиента из CryptoBot.
    PayXrInvoice,
}

impl Direction {
    pub const ALL: [Direction; 4] = [
        Direction::XrToCbCheck,
        Direction::CbToXrCheck,
        Direction::PayCbInvoice,
        Direction::PayXrInvoice,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Direction::XrToCbCheck => "xr_to_cb_check",
            Direction::CbToXrCheck => "cb_to_xr_check",
            Direction::PayCbInvoice => "pay_cb_invoice",
            Direction::PayXrInvoice => "pay_xr_invoice",
        }
    }

    pub const fn flow(self) -> Flow {
        match self {
            Direction::XrToCbCheck | Direction::CbToXrCheck => Flow::CheckExchange,
            Direction::PayCbInvoice | Direction::PayXrInvoice => Flow::InvoicePayment,
        }
    }

    /// Платформа, откуда приходят деньги клиента (и куда идёт возврат).
    pub const fn source(self) -> Platform {
        match self {
            Direction::XrToCbCheck | Direction::PayCbInvoice => Platform::XRocket,
            Direction::CbToXrCheck | Direction::PayXrInvoice => Platform::CryptoBot,
        }
    }

    /// Платформа выплаты или оплачиваемого счёта.
    pub const fn target(self) -> Platform {
        match self {
            Direction::XrToCbCheck | Direction::PayCbInvoice => Platform::CryptoBot,
            Direction::CbToXrCheck | Direction::PayXrInvoice => Platform::XRocket,
        }
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown direction code: {0}")]
pub struct UnknownDirection(pub String);

impl FromStr for Direction {
    type Err = UnknownDirection;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|d| d.code() == s)
            .ok_or_else(|| UnknownDirection(s.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip_and_sides_differ() {
        for d in Direction::ALL {
            assert_eq!(d.code().parse::<Direction>(), Ok(d));
            assert_ne!(d.source(), d.target());
        }
    }

    #[test]
    fn invoice_direction_pays_target_platform_invoice() {
        assert_eq!(Direction::PayCbInvoice.target(), Platform::CryptoBot);
        assert_eq!(Direction::PayCbInvoice.flow(), Flow::InvoicePayment);
        assert_eq!(Direction::XrToCbCheck.flow(), Flow::CheckExchange);
    }
}
