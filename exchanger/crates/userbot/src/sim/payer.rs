//! [`SimWebAppPayer`] — шаг PIN мини-приложения CryptoBot в симуляторе: вводит PIN через
//! [`SimWorld::complete_webapp_payment`]. Настоящий плательщик появится после записи HAR
//! владельцем (HANDOVER §7, п. 4).

use async_trait::async_trait;
use domain::Platform;

use super::{SimWorld, WebAppPayment};
use crate::flows::{WebAppOutcome, WebAppPayer};

/// Плательщик мини-приложения с заданным PIN.
#[derive(Clone)]
pub struct SimWebAppPayer {
    world: SimWorld,
    pin: String,
}

impl std::fmt::Debug for SimWebAppPayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SimWebAppPayer { pin: [REDACTED] }")
    }
}

impl SimWebAppPayer {
    pub fn new(world: SimWorld, pin: impl Into<String>) -> Self {
        Self {
            world,
            pin: pin.into(),
        }
    }
}

#[async_trait]
impl WebAppPayer for SimWebAppPayer {
    async fn pay(&self, platform: Platform, session_url: &str) -> WebAppOutcome {
        if platform != Platform::CryptoBot {
            return WebAppOutcome::NotPaid("mini app payment is CryptoBot only".to_owned());
        }
        match self.world.complete_webapp_payment(session_url, &self.pin) {
            Ok(WebAppPayment::Paid(_)) => WebAppOutcome::Paid,
            Ok(WebAppPayment::AlreadyPaid) => WebAppOutcome::AlreadyPaid,
            Ok(WebAppPayment::Expired) => WebAppOutcome::Expired,
            Ok(WebAppPayment::InsufficientFunds { .. }) => WebAppOutcome::InsufficientFunds,
            Ok(WebAppPayment::WrongPin { attempts_left }) => {
                WebAppOutcome::NotPaid(format!("wrong PIN, {attempts_left} attempts left"))
            }
            Ok(WebAppPayment::Blocked) => WebAppOutcome::NotPaid("PIN blocked".to_owned()),
            Ok(WebAppPayment::SessionUsed) => {
                WebAppOutcome::Unknown("mini app session was already used".to_owned())
            }
            Ok(WebAppPayment::OwnInvoice) => WebAppOutcome::NotPaid("own invoice".to_owned()),
            Err(e) => WebAppOutcome::NotPaid(e.to_string()),
        }
    }
}
