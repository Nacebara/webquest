//! Параметры методов Bot API. Каждый тип сериализуется ровно в JSON-тело запроса
//! (`None` и пустые поля не отправляются) и знает имя метода и тип ответа.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::keyboard::{InlineKeyboardMarkup, ReplyMarkup};
use crate::types::{BotCommand, BotCommandScope, Message, Update, UpdateKind, User};

/// Метод Bot API: имя и тип поля `result` в ответе.
pub trait Method: Serialize {
    const NAME: &'static str;
    type Response: DeserializeOwned;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ParseMode {
    #[serde(rename = "HTML")]
    Html,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct LinkPreviewOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_disabled: Option<bool>,
}

impl LinkPreviewOptions {
    pub const fn disabled() -> Self {
        Self {
            is_disabled: Some(true),
        }
    }
}

/// `rich_message` для `sendRichMessage` и `editMessageText` (Bot API 10.3).
/// `skip_entity_detection: true` — как у lovec: `@имена` и URL не превращаются в ссылки сами,
/// ссылки в разметке задаём явно.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RichMessage {
    pub html: String,
    pub skip_entity_detection: bool,
}

impl RichMessage {
    pub fn new(html: impl Into<String>) -> Self {
        Self {
            html: html.into(),
            skip_entity_detection: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct GetMe {}

impl Method for GetMe {
    const NAME: &'static str = "getMe";
    type Response = User;
}

/// Длинный опрос. `timeout` — секунды ожидания на стороне Telegram; клиент ждёт ответа
/// дольше на запас (см. `BotApi::get_updates`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GetUpdates {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_updates: Option<Vec<UpdateKind>>,
}

impl GetUpdates {
    /// Следующая порция после последнего обработанного `update_id`.
    pub fn after(last_update_id: Option<i64>, timeout_secs: u32) -> Self {
        Self {
            offset: last_update_id.map(|id| id.saturating_add(1)),
            limit: None,
            timeout: Some(timeout_secs),
            allowed_updates: None,
        }
    }

    pub fn allowed(mut self, kinds: Vec<UpdateKind>) -> Self {
        self.allowed_updates = Some(kinds);
        self
    }
}

impl Method for GetUpdates {
    const NAME: &'static str = "getUpdates";
    /// Разбирается по одному обновлению: битое не останавливает приём (см. `BotApi::get_updates`).
    type Response = Vec<serde_json::Value>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SendMessage {
    pub chat_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_thread_id: Option<i32>,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_mode: Option<ParseMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_preview_options: Option<LinkPreviewOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_notification: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protect_content: Option<bool>,
    /// Ответ-цитата на сообщение этого чата.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_parameters: Option<ReplyParameters>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_markup: Option<ReplyMarkup>,
}

/// `reply_parameters`: на какое сообщение отвечаем. Нет сообщения — отправить без ответа.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ReplyParameters {
    pub message_id: i32,
    pub allow_sending_without_reply: bool,
}

impl ReplyParameters {
    pub fn to(message_id: i32) -> Self {
        Self {
            message_id,
            allow_sending_without_reply: true,
        }
    }
}

impl SendMessage {
    /// HTML-сообщение без превью ссылок (как lovec `html_send_body`).
    pub fn html(chat_id: i64, text: impl Into<String>) -> Self {
        Self {
            chat_id,
            message_thread_id: None,
            text: text.into(),
            parse_mode: Some(ParseMode::Html),
            link_preview_options: Some(LinkPreviewOptions::disabled()),
            disable_notification: None,
            protect_content: None,
            reply_parameters: None,
            reply_markup: None,
        }
    }

    pub fn thread(mut self, thread_id: i32) -> Self {
        self.message_thread_id = Some(thread_id);
        self
    }

    pub fn silent(mut self) -> Self {
        self.disable_notification = Some(true);
        self
    }

    pub fn reply_markup(mut self, markup: impl Into<ReplyMarkup>) -> Self {
        self.reply_markup = Some(markup.into());
        self
    }

    /// Ответить цитатой на сообщение `message_id`.
    pub fn reply_to(mut self, message_id: i32) -> Self {
        self.reply_parameters = Some(ReplyParameters::to(message_id));
        self
    }
}

impl Method for SendMessage {
    const NAME: &'static str = "sendMessage";
    type Response = Message;
}

/// `sendRichMessage` (Bot API 10.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SendRichMessage {
    pub chat_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_thread_id: Option<i32>,
    pub rich_message: RichMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_notification: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protect_content: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_markup: Option<ReplyMarkup>,
}

impl SendRichMessage {
    pub fn new(chat_id: i64, html: impl Into<String>) -> Self {
        Self {
            chat_id,
            message_thread_id: None,
            rich_message: RichMessage::new(html),
            disable_notification: None,
            protect_content: None,
            reply_markup: None,
        }
    }

    pub fn thread(mut self, thread_id: i32) -> Self {
        self.message_thread_id = Some(thread_id);
        self
    }

    pub fn silent(mut self) -> Self {
        self.disable_notification = Some(true);
        self
    }

    pub fn reply_markup(mut self, markup: impl Into<ReplyMarkup>) -> Self {
        self.reply_markup = Some(markup.into());
        self
    }
}

impl Method for SendRichMessage {
    const NAME: &'static str = "sendRichMessage";
    type Response = Message;
}

/// Новое содержимое сообщения: обычный HTML или rich. Сообщение правится той же разметкой,
/// какой создано (урок lovec `LiveMessage::edit`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum EditContent {
    Html {
        text: String,
        parse_mode: ParseMode,
        #[serde(skip_serializing_if = "Option::is_none")]
        link_preview_options: Option<LinkPreviewOptions>,
    },
    Rich {
        rich_message: RichMessage,
    },
}

/// `editMessageText` сообщения в чате. Править можно только с инлайн-клавиатурой.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EditMessageText {
    pub chat_id: i64,
    pub message_id: i32,
    #[serde(flatten)]
    pub content: EditContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_markup: Option<InlineKeyboardMarkup>,
}

impl EditMessageText {
    pub fn html(chat_id: i64, message_id: i32, text: impl Into<String>) -> Self {
        Self {
            chat_id,
            message_id,
            content: EditContent::Html {
                text: text.into(),
                parse_mode: ParseMode::Html,
                link_preview_options: Some(LinkPreviewOptions::disabled()),
            },
            reply_markup: None,
        }
    }

    pub fn rich(chat_id: i64, message_id: i32, html: impl Into<String>) -> Self {
        Self {
            chat_id,
            message_id,
            content: EditContent::Rich {
                rich_message: RichMessage::new(html),
            },
            reply_markup: None,
        }
    }

    pub fn reply_markup(mut self, markup: InlineKeyboardMarkup) -> Self {
        self.reply_markup = Some(markup);
        self
    }
}

impl Method for EditMessageText {
    const NAME: &'static str = "editMessageText";
    type Response = Message;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AnswerCallbackQuery {
    pub callback_query_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_alert: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_time: Option<u32>,
}

impl AnswerCallbackQuery {
    /// Просто убрать «часики» на кнопке.
    pub fn ack(callback_query_id: impl Into<String>) -> Self {
        Self {
            callback_query_id: callback_query_id.into(),
            ..Self::default()
        }
    }

    /// Всплывающее уведомление; `alert` — окно с кнопкой «OK».
    pub fn notice(
        callback_query_id: impl Into<String>,
        text: impl Into<String>,
        alert: bool,
    ) -> Self {
        Self {
            callback_query_id: callback_query_id.into(),
            text: Some(text.into()),
            show_alert: alert.then_some(true),
            ..Self::default()
        }
    }
}

impl Method for AnswerCallbackQuery {
    const NAME: &'static str = "answerCallbackQuery";
    type Response = bool;
}

/// `forwardMessage`: переслать сообщение; ответ — пересланная копия со всеми кнопками.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ForwardMessage {
    pub chat_id: i64,
    pub from_chat_id: i64,
    pub message_id: i32,
    pub disable_notification: bool,
}

impl Method for ForwardMessage {
    const NAME: &'static str = "forwardMessage";
    type Response = Message;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DeleteMessage {
    pub chat_id: i64,
    pub message_id: i32,
}

impl Method for DeleteMessage {
    const NAME: &'static str = "deleteMessage";
    type Response = bool;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetMyCommands {
    pub commands: Vec<BotCommand>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<BotCommandScope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language_code: Option<String>,
}

impl SetMyCommands {
    pub fn new(commands: Vec<BotCommand>) -> Self {
        Self {
            commands,
            scope: None,
            language_code: None,
        }
    }
}

impl Method for SetMyCommands {
    const NAME: &'static str = "setMyCommands";
    type Response = bool;
}

/// Разобрать `result` метода `getUpdates` по одному обновлению. Битое обновление (его `update_id`
/// виден) превращается в [`Update::skipped`], чтобы `offset` сдвинулся и приём не встал;
/// без `update_id` — пропускается.
pub fn parse_updates(raw: Vec<serde_json::Value>) -> Vec<Update> {
    raw.into_iter()
        .filter_map(|value| {
            let id = value.get("update_id").and_then(serde_json::Value::as_i64);
            match serde_json::from_value::<Update>(value) {
                Ok(update) => Some(update),
                Err(_) => id.map(Update::skipped),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Наборы кастомных эмодзи (установка своего набора exch: `exch emoji install`)
// ---------------------------------------------------------------------------------------------

/// Формат файла стикера.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StickerFormat {
    /// PNG или WEBP; для кастомного эмодзи — 100×100.
    Static,
    /// TGS (Lottie).
    Animated,
}

impl StickerFormat {
    pub const fn name(self) -> &'static str {
        match self {
            StickerFormat::Static => "static",
            StickerFormat::Animated => "animated",
        }
    }
}

/// Стикер для `createNewStickerSet` / `addStickerToSet`: файл уже загружен
/// (`uploadStickerFile` → `file_id`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InputSticker {
    pub sticker: String,
    pub format: StickerFormat,
    pub emoji_list: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CreateNewStickerSet {
    /// Владелец набора — человек, который писал боту.
    pub user_id: i64,
    /// Имя набора: латиница, цифры, `_`, кончается на `_by_<имя бота>`.
    pub name: String,
    pub title: String,
    /// До 50 стикеров; остальные — `addStickerToSet`.
    pub stickers: Vec<InputSticker>,
    pub sticker_type: &'static str,
    /// Перекрашивать в цвет текста (линейные значки).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub needs_repainting: bool,
}

impl CreateNewStickerSet {
    pub fn custom_emoji(
        user_id: i64,
        name: impl Into<String>,
        title: impl Into<String>,
        stickers: Vec<InputSticker>,
        needs_repainting: bool,
    ) -> Self {
        Self {
            user_id,
            name: name.into(),
            title: title.into(),
            stickers,
            sticker_type: "custom_emoji",
            needs_repainting,
        }
    }
}

impl Method for CreateNewStickerSet {
    const NAME: &'static str = "createNewStickerSet";
    type Response = bool;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AddStickerToSet {
    pub user_id: i64,
    pub name: String,
    pub sticker: InputSticker,
}

impl Method for AddStickerToSet {
    const NAME: &'static str = "addStickerToSet";
    type Response = bool;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GetStickerSet {
    pub name: String,
}

impl Method for GetStickerSet {
    const NAME: &'static str = "getStickerSet";
    type Response = crate::types::StickerSet;
}
