//! Тонкий клиент Telegram Bot API для клиентского и ops-бота exch (DESIGN-v0.2 §6, SPEC §6.1).
//!
//! Образец — lovec `notifier.rs` `bot_call`: JSON-вызовы через `reqwest`, разбор
//! `ok`/`description`/`retry_after`, токен никогда не попадает в ошибки и логи. Поля Bot API
//! 10.3, которых нет в готовых крейтах: `sendRichMessage` и `editMessageText` с
//! `rich_message {html, skip_entity_detection: true}`, `style` и `icon_custom_emoji_id`
//! у кнопок, `copy_text`, `disabled`.
//!
//! Без фичи `client` крейт — только типы (их использует `ui`, которому сеть не нужна).

pub mod error;
pub mod keyboard;
pub mod methods;
pub mod types;

#[cfg(feature = "client")]
mod client;

#[cfg(feature = "client")]
pub use client::{
    BotApi, BotApiBuilder, DEFAULT_BASE_URL, DEFAULT_CONNECT_TIMEOUT, DEFAULT_TIMEOUT,
    LONG_POLL_GRACE,
};
pub use error::{Error, TransportKind};
pub use keyboard::{
    ButtonStyle, InlineButtonAction, InlineKeyboardButton, InlineKeyboardMarkup, KeyboardButton,
    ReplyKeyboardMarkup, ReplyKeyboardRemove, ReplyMarkup,
};
pub use methods::{
    AddStickerToSet, AnswerCallbackQuery, CreateNewStickerSet, DeleteMessage, EditContent,
    EditMessageText, ForwardMessage, GetMe, GetStickerSet, GetUpdates, InputSticker,
    LinkPreviewOptions, Method, ParseMode, ReplyParameters, RichMessage, SendMessage,
    SendRichMessage, SetMyCommands, StickerFormat,
};
pub use types::{
    BotCommand, BotCommandScope, CallbackQuery, Chat, ChatType, EntityKind, File, Message,
    MessageEntity, MessageOrigin, ResponseParameters, Sticker, StickerSet, Update, UpdateKind,
    User,
};
