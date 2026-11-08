//! Рендер экранов клиентского бота. [`Ui`] держит тексты, набор эмодзи и настройки времени;
//! каждый экран — чистая функция данных в [`Screen`].
//!
//! - `reference` — справочные экраны (rich + HTML-фолбэк там, где есть таблицы);
//! - `flow` — денежный поток одной заявки (только обычный HTML, SPEC §6.1);
//! - `errors` — отказы и ошибки (SPEC §6.3).

mod errors;
mod flow;
mod invest;
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

    /// Тот же интерфейс с другим набором эмодзи (пульт сменил вариацию или свой эмодзи).
    pub fn with_emoji(&self, emoji: EmojiSet) -> Self {
        Self::new(self.texts.clone(), emoji, self.config.clone())
    }

    /// Главное меню — макет B1: [Обменять] [Оплатить счёт] синие, ниже [Курс] [История]
    /// [Помощь]; подсказка в поле ввода «Пришлите чек или счёт». Цвет кнопок — всем (это не
    /// премиум), иконки `icon_custom_emoji_id` — при Premium владельца бота, иначе символ.
    pub fn main_menu(&self) -> ReplyKeyboardMarkup {
        let button = |action: MenuAction| {
            let (role, label) = self.menu_item(action);
            let (text, icon) = self.emoji.button(Some(role), label);
            let main = matches!(action, MenuAction::Exchange | MenuAction::PayInvoice);
            KeyboardButton::new(text)
                .maybe_style(main.then_some(ButtonStyle::Primary))
                .maybe_icon(icon)
        };
        ReplyKeyboardMarkup {
            keyboard: vec![
                vec![button(MenuAction::Exchange), button(MenuAction::PayInvoice)],
                vec![
                    button(MenuAction::Rates),
                    button(MenuAction::History),
                    button(MenuAction::Help),
                ],
            ],
            is_persistent: Some(true),
            resize_keyboard: Some(true),
            one_time_keyboard: None,
            input_field_placeholder: Some(self.texts.menu.placeholder.clone()),
        }
    }

    /// Текст сообщения → кнопка меню. Терпимо к эмодзи, регистру, «ё» и лишним пробелам;
    /// понимает и подписи прежних версий меню (`[menu].legacy`).
    pub fn parse_menu(&self, text: &str) -> Option<MenuAction> {
        let needle = normalize_menu_text(text);
        if needle.is_empty() {
            return None;
        }
        MenuAction::ALL
            .into_iter()
            .find(|action| normalize_menu_text(self.menu_item(*action).1) == needle)
            .or_else(|| {
                self.texts
                    .menu
                    .legacy
                    .iter()
                    .find(|l| normalize_menu_text(&l.text) == needle)
                    .and_then(|l| MenuAction::from_key(&l.action))
            })
    }

    fn menu_item(&self, action: MenuAction) -> (Emoji, &str) {
        let m = &self.texts.menu;
        match action {
            MenuAction::Exchange => (Emoji::Exchange, &m.exchange),
            MenuAction::PayInvoice => (Emoji::Invoice, &m.pay_invoice),
            MenuAction::Rates => (Emoji::Rates, &m.rates),
            MenuAction::History => (Emoji::History, &m.history),
            MenuAction::Support => (Emoji::Support, &m.support),
            MenuAction::Help => (Emoji::Help, &m.help),
            MenuAction::Invest => (Emoji::Invest, &m.invest),
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

    /// Клавиатура rich-экрана — как у экрана. Прежде бот дописывал «Не отображается?» к
    /// каждому rich-экрану; в макете B1 переход на простой вид — кнопка «Простой вид»
    /// только в «Курс и лимиты».
    pub fn rich_keyboard(&self, screen: &Screen) -> Keyboard {
        screen.keyboard.clone()
    }

    /// Справочный экран простым видом: тот же HTML, а «Простой вид» на клавиатуре заменён на
    /// «Красивый вид» — вернуть карточку.
    pub fn plain_screen(&self, screen: &Screen) -> Screen {
        let plain = Callback::PlainView.encode();
        let mut out = screen.clone();
        if let Keyboard::Inline(markup) = &mut out.keyboard {
            for button in markup.inline_keyboard.iter_mut().flatten() {
                if button.action == botapi::InlineButtonAction::CallbackData(plain.clone()) {
                    *button = self.cb_button(
                        Some(Emoji::RichView),
                        &self.texts.buttons.rich_view,
                        &Callback::RichView,
                    );
                }
            }
        }
        out
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

    /// Сумма как в макете: число моноширинным (копируется тапом), валюта — обычным текстом.
    fn money(&self, m: &Money) -> Frag {
        let mut f = Frag::code(&fmt::amount(m.amount(), m.asset()));
        f.push_text(&format!("\u{a0}{}", m.asset().code()));
        f
    }

    /// Только число моноширинным: «Пришлите ещё чек на <code>1.25</code>».
    fn amount_code(&self, m: &Money) -> Frag {
        Frag::code(&fmt::amount(m.amount(), m.asset()))
    }

    /// Только число обычным текстом: «Получено 51.25 · комиссия …».
    fn amount_text(&self, m: &Money) -> Frag {
        Frag::text(&fmt::amount(m.amount(), m.asset()))
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

    /// Кнопка `copy_text` без своей иконки: значок «копировать» Telegram рисует на ней сам —
    /// со второй иконкой их было два (решение владельца 8 ноября 2026).
    fn copy_button(&self, label: &str, copy: &str) -> InlineKeyboardButton {
        InlineKeyboardButton::copy_text(label, copy)
    }

    fn copy_order_button(&self, order: &str) -> InlineKeyboardButton {
        self.copy_button(&self.texts.buttons.copy_order, order)
    }

    /// «Забрать чек» (url, `success`) и «Скопировать ссылку» (`copy_text`) — DESIGN-v0.2 §6.
    fn claim_row(&self, check_url: &str) -> Vec<InlineKeyboardButton> {
        let b = &self.texts.buttons;
        vec![
            self.url_button(Some(Emoji::Claim), &b.claim_check, check_url)
                .style(ButtonStyle::Success),
            self.copy_button(&b.copy_link, check_url),
        ]
    }

    /// «Квитанция» (выбор владельца 8 ноября 2026): строки «подпись · сумма» столбиком
    /// моноширинным шрифтом, итог под чертой. `<pre>` есть и в rich, и в обычном HTML.
    /// `label` — подпись в шапке блока («Пример расчёта»): Telegram показывает её там, где у
    /// кода пишется язык. Одно слово на класс — пробелы заменяются неразрывными (проверено
    /// живьём 8 ноября 2026: так подпись приходит целиком).
    fn receipt(
        &self,
        label: Option<&str>,
        rows: &[(String, Decimal)],
        total: (&str, Decimal),
    ) -> Frag {
        let num = |v: Decimal| fmt::amount(v, Asset::Usdt);
        let all: Vec<(String, String)> = rows
            .iter()
            .map(|(l, v)| (l.clone(), num(*v)))
            .chain(std::iter::once((total.0.to_owned(), num(total.1))))
            .collect();
        let width = all
            .iter()
            .map(|(l, v)| l.chars().count() + v.chars().count() + 3)
            .max()
            .unwrap_or(0)
            .max(24);
        let line = |l: &str, v: &str| {
            let pad = width.saturating_sub(l.chars().count() + v.chars().count());
            format!("{l}{}{v}", " ".repeat(pad))
        };
        let mut out: Vec<String> = all[..all.len() - 1]
            .iter()
            .map(|(l, v)| line(l, v))
            .collect();
        // Черта — тире «—»: ровно в клетку в любом моноширинном шрифте и посередине строки.
        // «─» шрифт кода Telegram на Android (Roboto Mono) не знает: его рисует другой шрифт,
        // на 18 % шире, и черта вылезала за суммы (скриншот владельца 8 ноября 2026).
        out.push("—".repeat(width));
        if let Some((l, v)) = all.last() {
            out.push(line(l, v));
        }
        let text = out.join("\n");
        let body = crate::html::esc(&text);
        let html = match label {
            Some(label) => format!(
                "<pre><code class=\"language-{}\">{body}</code></pre>",
                crate::html::esc(&label.replace([' ', '"'], "\u{a0}"))
            ),
            None => format!("<pre>{body}</pre>"),
        };
        Frag::dual(html.clone(), html)
    }

    /// «Скопируйте, чтобы создать чек ⬇️» — над кнопкой с командой кошелька.
    fn copy_hint(&self) -> Frag {
        self.fill(&self.texts.receipt.copy_hint, &[])
    }

    /// Кнопка квитанции под сообщением: команда кошелька с суммой («@send 100usdt»,
    /// «@xrocket 51.50»), синяя, копируется нажатием (решение владельца 8 ноября 2026).
    fn command_button(&self, command: &str) -> InlineKeyboardButton {
        self.copy_button(command, command)
            .style(botapi::ButtonStyle::Primary)
    }

    /// Экран-квитанция: rich (абзацы, `<pre>`, подвал) и тот же обычный HTML запасом.
    /// Кнопок «Простой вид» нет: это шаг денежного потока, а не справка.
    fn receipt_screen(&self, doc: crate::doc::Doc, keyboard: Keyboard) -> Screen {
        self.dual(doc, keyboard)
    }

    fn button_text(&self, template: &str, args: &[(&str, &str)]) -> String {
        fill_text(template, args)
    }
}

/// Значок кошелька: CryptoBot — синий ромб, xRocket — ракета.
pub(crate) fn wallet_emoji(platform: Platform) -> Emoji {
    match platform {
        Platform::CryptoBot => Emoji::Cb,
        Platform::XRocket => Emoji::Xr,
    }
}

/// Инлайн-команда кошелька для создания чека (DESIGN-v0.2 Р5).
pub fn inline_command(platform: Platform, amount: Decimal) -> String {
    // Целая сумма — без копеек («@send 100usdt»), дробная — всегда два знака («@xrocket
    // 51.50»), как в квитанции.
    let amount = if amount.fract().is_zero() {
        fmt::bare_amount(amount)
    } else {
        fmt::amount(amount, domain::Asset::Usdt)
    };
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
