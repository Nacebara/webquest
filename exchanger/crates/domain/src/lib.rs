//! Доменная модель обменника: деньги, цены, машина состояний заявки, проводки.
//! Без IO, tokio и БД (SPEC §10.2). Деньги — только `rust_decimal::Decimal`.

pub mod asset;
pub mod direction;
pub mod fsm;
pub mod ledger;
pub mod money;
pub mod pricing;

pub use asset::{Asset, Platform, WalletKind, WalletRef};
pub use direction::Direction;
pub use fsm::{Event, Flow, OrderState, Transition};
pub use money::{Money, Step};
pub use rust_decimal::Decimal;
