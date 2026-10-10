//! Входящие объекты Bot API: обновления, сообщения, пользователи, чаты, нажатия кнопок.
//!
//! Разбор терпимый: незнакомые поля игнорируются (serde по умолчанию), незнакомые значения
//! перечислений (`type` чата, сущности, источника пересылки) превращаются в `Unknown`,
//! а не в ошибку. Иначе одно новое поле в Bot API остановило бы приём обновлений.

use serde::{Deserialize, Serialize};

use crate::keyboard::{InlineButtonAction, InlineKeyboardMarkup};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: i64,
    #[serde(default)]
    pub is_bot: bool,
    #[serde(default)]
    pub first_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_premium: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatType {
    Private,
    Group,
    Supergroup,
    Channel,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: ChatType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
}

/// Тип сущности разметки (`MessageEntity.type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Mention,
    Hashtag,
    Cashtag,
    BotCommand,
    Url,
    Email,
    PhoneNumber,
    Bold,
    Italic,
    Underline,
    Strikethrough,
    Spoiler,
    Blockquote,
    ExpandableBlockquote,
    Code,
    Pre,
    TextLink,
    TextMention,
    CustomEmoji,
    #[serde(other)]
    Unknown,
}

/// Сущность разметки. `offset` и `length` — в единицах UTF-16, как в Bot API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageEntity {
    #[serde(rename = "type")]
    pub kind: EntityKind,
    pub offset: u32,
    pub length: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_emoji_id: Option<String>,
}

/// Откуда переслано сообщение (`forward_origin`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageOrigin {
    User {
        date: i64,
        sender_user: User,
    },
    HiddenUser {
        date: i64,
        sender_user_name: String,
    },
    Chat {
        date: i64,
        sender_chat: Chat,
    },
    Channel {
        date: i64,
        chat: Chat,
        message_id: i32,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub message_id: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_thread_id: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<User>,
    pub chat: Chat,
    /// Unix-время. У недоступного сообщения в `CallbackQuery` — 0.
    #[serde(default)]
    pub date: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_date: Option<i64>,
    /// Сообщение отправлено через инлайн-режим бота (`@send 10usdt` в чате с нами).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_bot: Option<User>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forward_origin: Option<MessageOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<MessageEntity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caption_entities: Vec<MessageEntity>,
    /// Инлайн-клавиатура сообщения. У пересланного чека ссылка на него — в URL-кнопке.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_markup: Option<InlineKeyboardMarkup>,
}

impl Message {
    /// Текст сообщения или подпись к медиа и сущности к нему.
    pub fn text_and_entities(&self) -> Option<(&str, &[MessageEntity])> {
        if let Some(text) = &self.text {
            return Some((text, &self.entities));
        }
        self.caption
            .as_deref()
            .map(|caption| (caption, self.caption_entities.as_slice()))
    }

    /// URL из сущностей `url` (видимый текст ссылки) и `text_link` (скрытая ссылка), по порядку.
    /// Вместе с [`Message::button_urls`] — вход `parsers::links::extract_from_message`.
    pub fn entity_urls(&self) -> Vec<String> {
        let Some((text, entities)) = self.text_and_entities() else {
            return Vec::new();
        };
        entities
            .iter()
            .filter_map(|e| match e.kind {
                EntityKind::TextLink => e.url.clone(),
                EntityKind::Url => utf16_slice(text, e.offset, e.length),
                _ => None,
            })
            .collect()
    }

    /// URL инлайн-кнопок сообщения по рядам слева направо.
    pub fn button_urls(&self) -> Vec<String> {
        self.reply_markup
            .iter()
            .flat_map(|m| m.inline_keyboard.iter().flatten())
            .filter_map(|b| match &b.action {
                InlineButtonAction::Url(url) => Some(url.clone()),
                _ => None,
            })
            .collect()
    }

    /// Сообщение недоступно (так Bot API отдаёт старые сообщения в `CallbackQuery`).
    pub fn is_inaccessible(&self) -> bool {
        self.date == 0
    }
}

/// Подстрока по смещению и длине в единицах UTF-16. `None`, если границы вне текста или
/// режут суррогатную пару.
pub fn utf16_slice(text: &str, offset: u32, length: u32) -> Option<String> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let start = usize::try_from(offset).ok()?;
    let end = start.checked_add(usize::try_from(length).ok()?)?;
    String::from_utf16(units.get(start..end)?).ok()
}

/// Нажатие инлайн-кнопки.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    /// Сообщение с кнопкой; у старого сообщения Bot API отдаёт только чат, id и `date = 0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_message_id: Option<String>,
    #[serde(default)]
    pub chat_instance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

/// Виды обновлений для `allowed_updates`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateKind {
    Message,
    EditedMessage,
    CallbackQuery,
    MyChatMember,
}

/// Обновление. Виды, которые клиенту не нужны, приходят как обновление без полей —
/// его `update_id` всё равно сдвигает `offset`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    pub update_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_message: Option<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_query: Option<CallbackQuery>,
}

impl Update {
    /// Обновление, которое не удалось разобрать: только id, чтобы сдвинуть `offset`.
    pub fn skipped(update_id: i64) -> Self {
        Self {
            update_id,
            message: None,
            edited_message: None,
            callback_query: None,
        }
    }

    /// Чат, к которому относится обновление.
    pub fn chat_id(&self) -> Option<i64> {
        self.message
            .as_ref()
            .or(self.edited_message.as_ref())
            .map(|m| m.chat.id)
            .or_else(|| {
                self.callback_query
                    .as_ref()
                    .and_then(|q| q.message.as_ref())
                    .map(|m| m.chat.id)
            })
    }

    /// Пользователь, от которого пришло обновление.
    pub fn from(&self) -> Option<&User> {
        self.message
            .as_ref()
            .or(self.edited_message.as_ref())
            .and_then(|m| m.from.as_ref())
            .or_else(|| self.callback_query.as_ref().map(|q| &q.from))
    }
}

/// Команда для меню бота (`setMyCommands`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotCommand {
    pub command: String,
    pub description: String,
}

impl BotCommand {
    pub fn new(command: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            description: description.into(),
        }
    }
}

/// Область действия списка команд.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BotCommandScope {
    Default,
    AllPrivateChats,
    AllGroupChats,
    AllChatAdministrators,
    Chat { chat_id: i64 },
}

/// `parameters` в ответе с ошибкой.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseParameters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrate_to_chat_id: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_slice_handles_emoji_and_bounds() {
        // 🚀 — две единицы UTF-16.
        let text = "🚀 t.me/send?start=CQabcdefghij";
        assert_eq!(
            utf16_slice(text, 3, 28).as_deref(),
            Some("t.me/send?start=CQabcdefghij")
        );
        assert_eq!(utf16_slice(text, 1, 2), None, "режет суррогатную пару");
        assert_eq!(utf16_slice(text, 3, 100), None);
        assert_eq!(utf16_slice(text, u32::MAX, u32::MAX), None);
    }
}
