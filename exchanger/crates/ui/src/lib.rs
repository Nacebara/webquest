//! Интерфейс клиентского бота exch — чистый рендер без IO (DESIGN-v0.2 §6, SPEC §6).
//!
//! На входе — данные заявки и условия направлений, на выходе — [`Screen`]: HTML для
//! `sendMessage`/`editMessageText`, rich-HTML для `sendRichMessage` (только справочные экраны)
//! и клавиатура типами `botapi`. Отправку, правку «одного сообщения на заявку», лимиты на чат
//! и переключение на простой вид делает обвязка бота.
//!
//! Правила (CLAUDE.md, 16–17; SPEC §6.1, §6.3):
//! - тексты — в `ru.toml`, в коде только структура;
//! - все подстановки экранируются, теги — только разрешённые Telegram;
//! - денежный поток — обычный HTML; rich — справочные экраны, у каждого HTML-фолбэк;
//! - каждая ошибка говорит, где деньги клиента и что делать;
//! - эмодзи по ролям: при Premium — `<tg-emoji>` и `icon_custom_emoji_id`, иначе обычные.

pub mod callback;
pub mod command;
mod doc;
pub mod emoji;
pub mod fmt;
pub mod html;
pub mod model;
pub mod screen;
mod screens;
pub mod texts;

pub use callback::Callback;
pub use command::Command;
pub use emoji::{Emoji, EmojiError, EmojiSet};
pub use fmt::{Clock, TimeFormat};
pub use model::{
    Breakdown, DirectionStatus, DirectionTerms, Excess, FiatAmount, HistoryRow, InvoiceProblem,
    InvoiceQuote, OrderCard, OrderRef, RefundReason, Terms, UiError,
};
pub use screen::{Keyboard, MenuAction, Screen, normalize_menu_text};
pub use screens::{Ui, UiConfig, inline_command, menu_command, wallet_bot_url};
pub use texts::{Texts, TextsError};
