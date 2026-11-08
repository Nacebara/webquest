//! Библиотечная часть бинарника `exch` (SPEC §10.1, HANDOVER §5 M2.2, M2.3, M5):
//!
//! - [`config`] — переменные окружения, секреты в `SecretString`;
//! - [`supervisor`] — перезапуск упавших задач с backoff;
//! - [`session_store`] — сессия MTProto в `userbot_accounts` (только шифротекст);
//! - [`wallet_log`] — журнал сообщений ботов кошельков в `wallet_messages`;
//! - [`telegram`] — настройки подключения юзерботов из окружения;
//! - [`login`] — `exch login`; [`record`] — `exch record`;
//! - [`runtime`] — подключение юзерботов и сборка движка для `exch run`;
//! - [`emoji`] — премиум-эмодзи: вариация набора, свои эмодзи по ролям, установка (`exch emoji`);
//! - [`project`] — папка проекта: `.env` и `config.toml` ([`settings_file`]);
//! - [`setup`] — мастер `exch setup`; [`check`] — проверка `exch check`; [`db`] — база.

pub mod check;
pub mod config;
pub mod db;
pub mod emoji;
pub mod listen;
pub mod login;
pub mod project;
pub mod prompt;
pub mod record;
pub mod runtime;
pub mod session_store;
pub mod settings_file;
pub mod setup;
pub mod supervisor;
pub mod telegram;
pub mod wallet_log;
