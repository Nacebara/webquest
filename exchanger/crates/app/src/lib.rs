//! Библиотечная часть бинарника `exch` (SPEC §10.1, HANDOVER §5 M2.2, M2.3, M5):
//!
//! - [`config`] — переменные окружения, секреты в `SecretString`;
//! - [`supervisor`] — перезапуск упавших задач с backoff;
//! - [`session_store`] — сессия MTProto в `userbot_accounts` (только шифротекст);
//! - [`wallet_log`] — журнал сообщений ботов кошельков в `wallet_messages`;
//! - [`telegram`] — настройки подключения юзерботов из окружения;
//! - [`login`] — `exch login`; [`record`] — `exch record`;
//! - [`runtime`] — подключение юзерботов и сборка движка для `exch run`.

pub mod config;
pub mod login;
pub mod record;
pub mod runtime;
pub mod session_store;
pub mod supervisor;
pub mod telegram;
pub mod wallet_log;
