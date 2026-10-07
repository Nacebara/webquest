//! Разбор того, что приходит из Telegram: ссылки на чеки и счета, суммы, ответы ботов
//! кошельков @send (CryptoBot) и @xrocket (SPEC §1.2, §1.3, §10.6; факты — из lovec 0.4.3).
//!
//! Контракт для engine и userbot. Сигнатуры публичных функций и варианты перечислений
//! менять только вместе с вызывающим кодом; новые варианты добавлять можно.

pub mod amount;
pub mod links;
pub mod replies;

pub use links::{LinkError, LinkKind, WalletLink};
pub use replies::{
    ActivationReply, BalanceLine, CreatedCheck, InvoiceCard, InvoicePayReply, InvoiceStatus,
};
