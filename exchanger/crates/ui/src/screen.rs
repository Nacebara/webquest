//! Результат рендера: текст в двух видах и клавиатура типами Bot API.

use botapi::{InlineKeyboardMarkup, ReplyKeyboardMarkup, ReplyKeyboardRemove, ReplyMarkup};

/// Клавиатура сообщения.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Keyboard {
    #[default]
    None,
    /// Инлайн-кнопки под сообщением. Только такую можно поставить при `editMessageText`.
    Inline(InlineKeyboardMarkup),
    /// Reply-клавиатура (главное меню).
    Reply(ReplyKeyboardMarkup),
    /// Снять reply-клавиатуру.
    Remove,
}

impl Keyboard {
    /// `reply_markup` для `sendMessage` / `sendRichMessage`.
    pub fn reply_markup(&self) -> Option<ReplyMarkup> {
        match self {
            Keyboard::None => None,
            Keyboard::Inline(m) => Some(ReplyMarkup::Inline(m.clone())),
            Keyboard::Reply(m) => Some(ReplyMarkup::Keyboard(m.clone())),
            Keyboard::Remove => Some(ReplyMarkup::Remove(ReplyKeyboardRemove::default())),
        }
    }

    /// Инлайн-клавиатура для `editMessageText`.
    pub fn inline(&self) -> Option<&InlineKeyboardMarkup> {
        match self {
            Keyboard::Inline(m) => Some(m),
            _ => None,
        }
    }

    pub(crate) fn inline_rows(rows: Vec<Vec<botapi::InlineKeyboardButton>>) -> Self {
        let rows: Vec<_> = rows.into_iter().filter(|r| !r.is_empty()).collect();
        if rows.is_empty() {
            Keyboard::None
        } else {
            Keyboard::Inline(InlineKeyboardMarkup::new(rows))
        }
    }
}

/// Готовый экран.
///
/// `rich_html` есть только у справочных экранов: его отправляют `sendRichMessage`, а при
/// отказе Telegram или у клиента с «простым видом» — `html` через `sendMessage` (SPEC §6.1).
/// Денежный поток — всегда только `html`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    pub html: String,
    pub rich_html: Option<String>,
    pub keyboard: Keyboard,
}

impl Screen {
    pub fn is_rich(&self) -> bool {
        self.rich_html.is_some()
    }
}

/// Кнопки главного меню.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MenuAction {
    Exchange,
    PayInvoice,
    Rates,
    History,
    Support,
}

impl MenuAction {
    pub const ALL: [MenuAction; 5] = [
        MenuAction::Exchange,
        MenuAction::PayInvoice,
        MenuAction::Rates,
        MenuAction::History,
        MenuAction::Support,
    ];
}

/// Текст кнопки меню без эмодзи, знаков и регистра: «📈 Курс и лимиты» → «курс и лимиты».
/// Клиент мог нажать кнопку старой клавиатуры (с эмодзи или без) или набрать текст руками.
pub fn normalize_menu_text(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| match c {
            'ё' | 'Ё' => 'е',
            c if c.is_alphanumeric() => c,
            _ => ' ',
        })
        .flat_map(char::to_lowercase)
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_drops_emoji_case_and_punctuation() {
        assert_eq!(normalize_menu_text("📈 Курс и лимиты"), "курс и лимиты");
        assert_eq!(normalize_menu_text("  КУРС   И ЛИМИТЫ!! "), "курс и лимиты");
        assert_eq!(normalize_menu_text("🧾 Оплатить счёт"), "оплатить счет");
        assert_eq!(normalize_menu_text("⚠️💬"), "");
    }
}
