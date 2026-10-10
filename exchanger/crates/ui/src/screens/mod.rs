//! Рендер экранов клиентского бота. [`Ui`] держит тексты, набор эмодзи и настройки времени;
//! каждый экран — чистая функция данных в [`Screen`].
//!
//! - `reference` — справочные экраны (rich + HTML-фолбэк там, где есть таблицы);
//! - `flow` — денежный поток одной заявки (только обычный HTML, SPEC §6.1);
//! - `errors` — отказы и ошибки (SPEC §6.3).

mod errors;
mod flow;
mod reference;

use botapi::{BotCommand, ButtonStyle, InlineKeyboardButton, KeyboardButton, ReplyKeyboardMarkup};
use chrono::{DateTime, Utc};
use domain::{Asset, Decimal, Direction, Money, Platform};

use crate::callback::Callback;
use crate::command::Command;
use crate::doc::Doc;
use crate::emoji::{Emoji, EmojiSet};
use crate::fmt::{self, Clock, TimeFormat};
use crate::html::{Frag, fill, fill_text};
use crate::screen::{Keyboard, MenuAction, Screen, normalize_menu_text};
use crate::texts::{Texts, TextsError};

/// Настройки рендера, не зависящие от текстов.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiConfig {
    /// Пояс запасного текста времени.
    pub clock: Clock,
    /// Ставить `<tg-time>` в обычный HTML (SPEC §6.1). Выключатель на случай, если Bot API не
    /// примет тег в HTML-режиме: тогда в сообщении будет только текст времени.
    pub tg_time_in_html: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            clock: Clock::msk(),
            tg_time_in_html: true,
        }
    }
}

/// Рендер интерфейса клиента.
#[derive(Debug, Clone)]
pub struct Ui {
    texts: Texts,
    emoji: EmojiSet,
    config: UiConfig,
}

impl Ui {
    pub fn new(texts: Texts, emoji: EmojiSet, config: UiConfig) -> Self {
        Self {
            texts,
            emoji,
            config,
        }
    }

    /// Со встроенными русскими текстами.
    pub fn builtin(emoji: EmojiSet, config: UiConfig) -> Result<Self, TextsError> {
        Ok(Self::new(Texts::builtin_ru()?, emoji, config))
    }

    pub fn texts(&self) -> &Texts {
        &self.texts
    }

    pub fn emoji(&self) -> &EmojiSet {
        &self.emoji
    }

    /// Главное меню: reply-клавиатура, постоянная и компактная (DESIGN-v0.2 §6). При Premium —
    /// иконки `icon_custom_emoji_id` вместо эмодзи в тексте и `primary` у главных действий.
    pub fn main_menu(&self) -> ReplyKeyboardMarkup {
        let premium = self.emoji.is_premium();
        let button = |action: MenuAction| {
            let (role, label) = self.menu_item(action);
            let (text, icon) = self.emoji.button(Some(role), label);
            let main = matches!(action, MenuAction::Exchange | MenuAction::PayInvoice);
            KeyboardButton::new(text)
                .maybe_style((premium && main).then_some(ButtonStyle::Primary))
                .maybe_icon(icon)
        };
        ReplyKeyboardMarkup {
            keyboard: vec![
                vec![button(MenuAction::Exchange), button(MenuAction::PayInvoice)],
                vec![button(MenuAction::Rates), button(MenuAction::History)],
                vec![button(MenuAction::Support)],
            ],
            is_persistent: Some(true),
            resize_keyboard: Some(true),
            one_time_keyboard: None,
            input_field_placeholder: Some(self.texts.menu.placeholder.clone()),
        }
    }

    /// Текст сообщения → кнопка меню. Терпимо к эмодзи, регистру, «ё» и лишним пробелам.
    pub fn parse_menu(&self, text: &str) -> Option<MenuAction> {
        let needle = normalize_menu_text(text);
        if needle.is_empty() {
            return None;
        }
        MenuAction::ALL
            .into_iter()
            .find(|action| normalize_menu_text(self.menu_item(*action).1) == needle)
    }

    fn menu_item(&self, action: MenuAction) -> (Emoji, &str) {
        let m = &self.texts.menu;
        match action {
            MenuAction::Exchange => (Emoji::Exchange, &m.exchange),
            MenuAction::PayInvoice => (Emoji::Invoice, &m.pay_invoice),
            MenuAction::Rates => (Emoji::Rates, &m.rates),
            MenuAction::History => (Emoji::History, &m.history),
            MenuAction::Support => (Emoji::Support, &m.support),
        }
    }

    /// Команды для `setMyCommands` (меню-кнопка бота).
    pub fn bot_commands(&self) -> Vec<BotCommand> {
        let c = &self.texts.commands;
        Command::NAMES
            .into_iter()
            .zip([&c.start, &c.terms, &c.history, &c.help, &c.privacy])
            .map(|(name, description)| BotCommand::new(name, description.as_str()))
            .collect()
    }

    /// Клавиатура для отправки экрана rich-сообщением: к инлайн-кнопкам добавляется
    /// «Не отображается?» (SPEC §6.1). Reply-клавиатуру не трогаем — смешивать нельзя.
    pub fn rich_keyboard(&self, screen: &Screen) -> Keyboard {
        let button = self.cb_button(None, &self.texts.buttons.not_rendered, &Callback::PlainView);
        match &screen.keyboard {
            Keyboard::Inline(markup) => {
                let mut rows = markup.inline_keyboard.clone();
                rows.push(vec![button]);
                Keyboard::inline_rows(rows)
            }
            Keyboard::None => Keyboard::inline_rows(vec![vec![button]]),
            other => other.clone(),
        }
    }

    // ---- общие кусочки ------------------------------------------------------------------

    fn fill(&self, template: &str, args: &[(&str, &Frag)]) -> Frag {
        fill(template, args, &self.emoji)
    }

    fn wallet_name(&self, platform: Platform) -> &str {
        self.texts.wallets.get(platform)
    }

    fn wallet(&self, platform: Platform) -> Frag {
        Frag::text(self.wallet_name(platform))
    }

    fn direction_name(&self, direction: Direction) -> Frag {
        Frag::text(self.texts.directions.get(direction))
    }

    fn money(&self, m: &Money) -> Frag {
        Frag::code(&fmt::money(m))
    }

    fn usdt(&self, value: Decimal) -> Frag {
        Frag::code(&fmt::amount(value, Asset::Usdt))
    }

    fn pct(&self, value: Decimal) -> Frag {
        Frag::text(&fmt::percent(value))
    }

    fn at(&self, at: DateTime<Utc>, format: TimeFormat) -> Frag {
        self.config
            .clock
            .frag(at, format, self.config.tg_time_in_html)
    }

    fn order_id(&self, id: &str) -> Frag {
        Frag::code(id)
    }

    /// Экран денежного потока: только обычный HTML.
    fn plain(&self, body: Frag, keyboard: Keyboard) -> Screen {
        let mut html = body.into_plain();
        let len = html.trim_end().len();
        html.truncate(len);
        Screen {
            html,
            rich_html: None,
            keyboard,
        }
    }

    /// Справочный экран: rich и HTML-фолбэк из одного документа.
    fn dual(&self, doc: Doc, keyboard: Keyboard) -> Screen {
        let (rich, html) = doc.finish();
        Screen {
            html,
            rich_html: Some(rich),
            keyboard,
        }
    }

    fn cb_button(
        &self,
        role: Option<Emoji>,
        label: &str,
        callback: &Callback,
    ) -> InlineKeyboardButton {
        let (text, icon) = self.emoji.button(role, label);
        InlineKeyboardButton::callback(text, callback.encode()).maybe_icon(icon)
    }

    fn url_button(&self, role: Option<Emoji>, label: &str, url: &str) -> InlineKeyboardButton {
        let (text, icon) = self.emoji.button(role, label);
        InlineKeyboardButton::url(text, url).maybe_icon(icon)
    }

    /// Кнопка `copy_text` с иконкой «копировать» (SPEC §6.4 `icon_copy`).
    fn copy_button(&self, label: &str, copy: &str) -> InlineKeyboardButton {
        let (text, icon) = self.emoji.button(Some(Emoji::Copy), label);
        InlineKeyboardButton::copy_text(text, copy).maybe_icon(icon)
    }

    fn copy_order_button(&self, order: &str) -> InlineKeyboardButton {
        self.copy_button(&self.texts.buttons.copy_order, order)
    }

    /// «Забрать чек» (url, `success`) и «Скопировать ссылку» (`copy_text`) — DESIGN-v0.2 §6.
    fn claim_row(&self, check_url: &str) -> Vec<InlineKeyboardButton> {
        let b = &self.texts.buttons;
        vec![
            self.url_button(None, &b.claim_check, check_url)
                .style(ButtonStyle::Success),
            self.copy_button(&b.copy_link, check_url),
        ]
    }

    fn button_text(&self, template: &str, args: &[(&str, &str)]) -> String {
        fill_text(template, args)
    }
}

/// Инлайн-команда кошелька для создания чека (DESIGN-v0.2 Р5).
pub fn inline_command(platform: Platform, amount: Decimal) -> String {
    let amount = fmt::bare_amount(amount);
    match platform {
        Platform::CryptoBot => format!("@send {amount}usdt"),
        Platform::XRocket => format!("@xrocket {amount}"),
    }
}

/// Команда меню чеков кошелька (запасной путь DESIGN-v0.2 Р5).
pub fn menu_command(platform: Platform) -> &'static str {
    match platform {
        Platform::CryptoBot => "/checks",
        Platform::XRocket => "/cheques",
    }
}

/// Ссылка на бота кошелька.
pub fn wallet_bot_url(platform: Platform) -> &'static str {
    match platform {
        Platform::CryptoBot => "https://t.me/send",
        Platform::XRocket => "https://t.me/xrocket",
    }
}

/// Имя пользователя Telegram: 3–32 символа `[A-Za-z0-9_]` (как lovec `is_username`).
pub(crate) fn username_ok(name: &str) -> bool {
    (3..=32).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
