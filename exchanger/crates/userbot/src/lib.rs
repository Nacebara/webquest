//! Юзербот exch (SPEC v0.2): вся работа с @send (CryptoBot) и @xrocket — через обычный аккаунт.
//!
//! - [`transport`] — трейт транспорта (реальный — крейт `mtproto`, тестовый — `sim`);
//! - [`wallet`] — операции кошелька для движка (`WalletBot`);
//! - `flows` — сценарии: активация чека, создание чека (инлайн и меню), счёт, баланс;
//! - `sim` (feature `sim`) — симулятор @send и @xrocket для тестов.

pub mod flows;
#[cfg(feature = "sim")]
pub mod sim;
pub mod transport;
pub mod wallet;

pub use flows::{AccountGate, FlowConfig, MessageLog, UserbotWallet, WebAppOutcome, WebAppPayer};
pub use transport::{Chat, RawMessage, Transport, TransportError};
pub use wallet::{
    ActivationOutcome, BotError, IssueMethod, IssueOutcome, IssuedCheck, OpTag, PayOutcome,
    RejectReason, WalletBot,
};
