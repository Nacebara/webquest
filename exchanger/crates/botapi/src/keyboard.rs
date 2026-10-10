//! Клавиатуры Bot API 10.3: инлайн-кнопки со стилем, иконкой из кастомного эмодзи,
//! `copy_text` и `disabled`; reply-клавиатура главного меню; её снятие.
//!
//! Инлайн-кнопка в Bot API — объект, где ровно одно поле задаёт действие. Здесь действие —
//! перечисление [`InlineButtonAction`], поэтому кнопку без действия или с двумя собрать нельзя.
//! На проводе — плоский объект (`{"text":…,"url":…}`), разбор входящих кнопок терпимый:
//! незнакомое действие (`web_app`, `login_url`, `pay`…) становится `Other`, незнакомый стиль
//! пропадает.

use serde::{Deserialize, Serialize};

/// Цвет кнопки (`style`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ButtonStyle {
    /// Синяя — основное действие.
    Primary,
    /// Зелёная — успешный итог («Забрать чек»).
    Success,
    /// Красная — опасное действие («Вернуть деньги»).
    Danger,
}

impl ButtonStyle {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "primary" => Some(Self::Primary),
            "success" => Some(Self::Success),
            "danger" => Some(Self::Danger),
            _ => None,
        }
    }
}

/// Что делает инлайн-кнопка.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InlineButtonAction {
    Url(String),
    /// 1–64 байта.
    CallbackData(String),
    /// Скопировать текст в буфер (1–256 символов).
    CopyText(String),
    /// Недоступная кнопка (Bot API 10.3): видна, но не нажимается.
    Disabled,
    /// Входящая кнопка с действием, которое этот клиент не моделирует.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "WireInlineButton", into = "WireInlineButton")]
pub struct InlineKeyboardButton {
    pub text: String,
    pub style: Option<ButtonStyle>,
    /// Иконка из кастомного эмодзи; работает, если у владельца бота есть Telegram Premium.
    pub icon_custom_emoji_id: Option<String>,
    pub action: InlineButtonAction,
}

impl InlineKeyboardButton {
    pub fn new(text: impl Into<String>, action: InlineButtonAction) -> Self {
        Self {
            text: text.into(),
            style: None,
            icon_custom_emoji_id: None,
            action,
        }
    }

    pub fn url(text: impl Into<String>, url: impl Into<String>) -> Self {
        Self::new(text, InlineButtonAction::Url(url.into()))
    }

    pub fn callback(text: impl Into<String>, data: impl Into<String>) -> Self {
        Self::new(text, InlineButtonAction::CallbackData(data.into()))
    }

    pub fn copy_text(text: impl Into<String>, copy: impl Into<String>) -> Self {
        Self::new(text, InlineButtonAction::CopyText(copy.into()))
    }

    pub fn disabled(text: impl Into<String>) -> Self {
        Self::new(text, InlineButtonAction::Disabled)
    }

    pub fn style(mut self, style: ButtonStyle) -> Self {
        self.style = Some(style);
        self
    }

    pub fn maybe_style(mut self, style: Option<ButtonStyle>) -> Self {
        self.style = style;
        self
    }

    pub fn icon(mut self, custom_emoji_id: impl Into<String>) -> Self {
        self.icon_custom_emoji_id = Some(custom_emoji_id.into());
        self
    }

    pub fn maybe_icon(mut self, custom_emoji_id: Option<String>) -> Self {
        self.icon_custom_emoji_id = custom_emoji_id;
        self
    }
}

/// `{"text": "…"}` внутри `copy_text`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CopyTextButton {
    text: String,
}

/// Кнопка как она лежит в JSON: все действия — необязательные поля.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct WireInlineButton {
    text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon_custom_emoji_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    callback_data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    copy_text: Option<CopyTextButton>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    disabled: Option<bool>,
}

impl From<WireInlineButton> for InlineKeyboardButton {
    fn from(w: WireInlineButton) -> Self {
        let action = if let Some(url) = w.url {
            InlineButtonAction::Url(url)
        } else if let Some(data) = w.callback_data {
            InlineButtonAction::CallbackData(data)
        } else if let Some(copy) = w.copy_text {
            InlineButtonAction::CopyText(copy.text)
        } else if w.disabled == Some(true) {
            InlineButtonAction::Disabled
        } else {
            InlineButtonAction::Other
        };
        Self {
            text: w.text,
            style: w.style.as_deref().and_then(ButtonStyle::parse),
            icon_custom_emoji_id: w.icon_custom_emoji_id,
            action,
        }
    }
}

impl From<InlineKeyboardButton> for WireInlineButton {
    fn from(b: InlineKeyboardButton) -> Self {
        let mut w = WireInlineButton {
            text: b.text,
            style: b.style.map(|s| style_name(s).to_owned()),
            icon_custom_emoji_id: b.icon_custom_emoji_id,
            ..WireInlineButton::default()
        };
        match b.action {
            InlineButtonAction::Url(url) => w.url = Some(url),
            InlineButtonAction::CallbackData(data) => w.callback_data = Some(data),
            InlineButtonAction::CopyText(text) => w.copy_text = Some(CopyTextButton { text }),
            InlineButtonAction::Disabled => w.disabled = Some(true),
            // Отправлять такую кнопку незачем; без действия Telegram её отвергнет.
            InlineButtonAction::Other => {}
        }
        w
    }
}

const fn style_name(style: ButtonStyle) -> &'static str {
    match style {
        ButtonStyle::Primary => "primary",
        ButtonStyle::Success => "success",
        ButtonStyle::Danger => "danger",
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineKeyboardMarkup {
    #[serde(default)]
    pub inline_keyboard: Vec<Vec<InlineKeyboardButton>>,
}

impl InlineKeyboardMarkup {
    pub fn new(rows: Vec<Vec<InlineKeyboardButton>>) -> Self {
        Self {
            inline_keyboard: rows,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.inline_keyboard.iter().all(Vec::is_empty)
    }

    /// Все кнопки по рядам слева направо.
    pub fn buttons(&self) -> impl Iterator<Item = &InlineKeyboardButton> {
        self.inline_keyboard.iter().flatten()
    }
}

/// Кнопка reply-клавиатуры: нажатие отправляет `text` обычным сообщением.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "WireKeyboardButton", into = "WireKeyboardButton")]
pub struct KeyboardButton {
    pub text: String,
    pub style: Option<ButtonStyle>,
    pub icon_custom_emoji_id: Option<String>,
}

impl KeyboardButton {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: None,
            icon_custom_emoji_id: None,
        }
    }

    pub fn maybe_style(mut self, style: Option<ButtonStyle>) -> Self {
        self.style = style;
        self
    }

    pub fn maybe_icon(mut self, custom_emoji_id: Option<String>) -> Self {
        self.icon_custom_emoji_id = custom_emoji_id;
        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct WireKeyboardButton {
    text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon_custom_emoji_id: Option<String>,
}

impl From<WireKeyboardButton> for KeyboardButton {
    fn from(w: WireKeyboardButton) -> Self {
        Self {
            text: w.text,
            style: w.style.as_deref().and_then(ButtonStyle::parse),
            icon_custom_emoji_id: w.icon_custom_emoji_id,
        }
    }
}

impl From<KeyboardButton> for WireKeyboardButton {
    fn from(b: KeyboardButton) -> Self {
        Self {
            text: b.text,
            style: b.style.map(|s| style_name(s).to_owned()),
            icon_custom_emoji_id: b.icon_custom_emoji_id,
        }
    }
}

/// Reply-клавиатура (главное меню клиента).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyKeyboardMarkup {
    pub keyboard: Vec<Vec<KeyboardButton>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_persistent: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resize_keyboard: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub one_time_keyboard: Option<bool>,
    /// Подсказка в поле ввода, 1–64 символа.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_field_placeholder: Option<String>,
}

/// Снять reply-клавиатуру.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyKeyboardRemove {
    /// Всегда `true`.
    pub remove_keyboard: bool,
}

impl Default for ReplyKeyboardRemove {
    fn default() -> Self {
        Self {
            remove_keyboard: true,
        }
    }
}

/// `reply_markup` исходящего сообщения.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum ReplyMarkup {
    Inline(InlineKeyboardMarkup),
    Keyboard(ReplyKeyboardMarkup),
    Remove(ReplyKeyboardRemove),
}

impl From<InlineKeyboardMarkup> for ReplyMarkup {
    fn from(m: InlineKeyboardMarkup) -> Self {
        Self::Inline(m)
    }
}

impl From<ReplyKeyboardMarkup> for ReplyMarkup {
    fn from(m: ReplyKeyboardMarkup) -> Self {
        Self::Keyboard(m)
    }
}

impl From<ReplyKeyboardRemove> for ReplyMarkup {
    fn from(m: ReplyKeyboardRemove) -> Self {
        Self::Remove(m)
    }
}
