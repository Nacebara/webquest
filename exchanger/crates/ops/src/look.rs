//! Вид пульта: премиум-эмодзи по макету в текстах и на кнопках.
//!
//! В текстах `ops/ru.toml` и в карточках значки пишутся токенами по ключу набора — как в
//! макете: `{e:alarm}` — цветной (`color/alarm`), `{m:queue}` — линейный (`mono/queue`).
//! Перед отправкой [`Look::screen`] меняет токены на `<tg-emoji>` или, без Premium, на обычный
//! символ ([`ui::fallback_char`]). Кнопка с подписью `"{m:queue} Разбор · 2"` получает иконку
//! `icon_custom_emoji_id`, а без Premium — символ перед текстом.
//!
//! Набор — тот же, что у клиентского бота: [`tgbot::emoji`] из базы, перечитывается сам.

use botapi::InlineKeyboardButton;
use ui::EmojiSet;

use crate::Screen;

/// Токен значка в тексте: (вид набора, имя).
fn token(s: &str) -> Option<(&'static str, &str, usize)> {
    let rest = s.strip_prefix('{')?;
    let (kind, rest) = if let Some(r) = rest.strip_prefix("e:") {
        ("color", r)
    } else {
        ("mono", rest.strip_prefix("m:")?)
    };
    let end = rest.find('}')?;
    let name = &rest[..end];
    let ok = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    // `{` + `e:` + имя + `}`
    ok.then_some((kind, name, 1 + 2 + end + 1))
}

/// Обычный HTML экрана → rich: абзацы через пустую строку, переводы строк — `<br/>`.
pub(crate) fn auto_rich(html: &str) -> String {
    html.split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .map(|p| format!("<p>{}</p>", p.trim().replace('\n', "<br/>")))
        .collect()
}

/// `<tg-button …>текст</tg-button>` → `текст` (для обычного HTML).
pub(crate) fn strip_text_buttons(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find("<tg-button") {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(open_end) = tail.find('>') else {
            out.push_str(tail);
            return out;
        };
        let inner = &tail[open_end + 1..];
        match inner.find("</tg-button>") {
            Some(close) => {
                out.push_str(&inner[..close]);
                rest = &inner[close + "</tg-button>".len()..];
            }
            None => {
                out.push_str(inner);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Вид пульта: действующий набор эмодзи.
#[derive(Debug, Clone, Default)]
pub struct Look {
    emoji: EmojiSet,
}

impl Look {
    pub fn new(emoji: EmojiSet) -> Self {
        Self { emoji }
    }

    /// Набор эмодзи вида (тот же для экранов клиента в «Предпросмотре»).
    pub fn emoji(&self) -> &EmojiSet {
        &self.emoji
    }

    /// Без Premium: обычные символы.
    pub fn plain() -> Self {
        Self::default()
    }

    fn id(&self, kind: &str, name: &str) -> Option<&str> {
        self.emoji.pack_id(&format!("{kind}/{name}"))
    }

    /// Значок в тексте: `<tg-emoji>` или обычный символ.
    pub fn icon(&self, kind: &str, name: &str) -> String {
        let plain = ui::fallback_char(name);
        match self.id(kind, name).or_else(|| self.id("mono", name)) {
            Some(id) => format!("<tg-emoji emoji-id=\"{id}\">{plain}</tg-emoji>"),
            None => plain.to_owned(),
        }
    }

    /// Заменить токены `{e:…}` / `{m:…}` значками.
    pub fn text(&self, html: &str) -> String {
        let mut out = String::with_capacity(html.len() + 64);
        let mut rest = html;
        while let Some(i) = rest.find('{') {
            out.push_str(&rest[..i]);
            let tail = &rest[i..];
            match token(tail) {
                Some((kind, name, len)) => {
                    out.push_str(&self.icon(kind, name));
                    rest = &tail[len..];
                }
                None => {
                    out.push('{');
                    rest = &tail[1..];
                }
            }
        }
        out.push_str(rest);
        out
    }

    /// Кнопка `"{m:queue} Разбор"`: иконка из набора или символ перед текстом.
    pub fn button(&self, mut b: InlineKeyboardButton) -> InlineKeyboardButton {
        let Some((kind, name, len)) = token(&b.text) else {
            b.text = self.text(&b.text);
            return b;
        };
        let label = b.text[len..].trim_start().to_owned();
        match self.id(kind, name).or_else(|| self.id("mono", name)) {
            Some(id) => {
                b.text = label;
                b.icon_custom_emoji_id = Some(id.to_owned());
            }
            None => b.text = format!("{} {label}", ui::fallback_char(name)),
        }
        b
    }

    /// Карточка «Статистики» со значками: текст карточки и кнопки.
    pub fn stats(&self, mut card: crate::stats::StatsCard) -> crate::stats::StatsCard {
        card.html = self.text(&card.html);
        card.keyboard = botapi::InlineKeyboardMarkup::new(
            card.keyboard
                .inline_keyboard
                .into_iter()
                .map(|row| row.into_iter().map(|b| self.button(b)).collect())
                .collect(),
        );
        card
    }

    /// Экран со значками: текст, карточка и кнопки.
    pub fn screen(&self, s: &Screen) -> Screen {
        // Экран без карточки, но с нажимаемыми строками — карточкой: кнопки в тексте живут
        // только в rich-сообщении.
        let rich = match s.rich.as_deref() {
            Some(r) => Some(r.to_owned()),
            None if s.text.contains("<tg-button") => Some(auto_rich(&s.text)),
            None => None,
        };
        Screen {
            // В обычном HTML кнопок в тексте (`<tg-button>`) не бывает — остаётся их текст.
            text: self.text(&strip_text_buttons(&s.text)),
            rich: rich.as_deref().map(|r| self.text(r)),
            keyboard: s
                .keyboard
                .iter()
                .map(|row| row.iter().cloned().map(|b| self.button(b)).collect())
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn premium() -> Look {
        let mut packs = ui::EmojiPacks::default();
        packs.animated.insert("color/alarm".into(), "111".into());
        packs.animated.insert("mono/queue".into(), "222".into());
        let settings = ui::EmojiSettings {
            style: ui::EmojiStyle::Animated,
            packs,
            custom: Default::default(),
        };
        Look::new(EmojiSet::from_settings(true, &settings).unwrap())
    }

    #[test]
    fn tokens_become_custom_emoji_or_plain_symbols() {
        let p = premium();
        assert_eq!(
            p.text("{e:alarm} Нужно решить · {n}"),
            "<tg-emoji emoji-id=\"111\">🚨</tg-emoji> Нужно решить · {n}"
        );
        // Цветного нет — берём линейный; нет никакого — символ.
        assert_eq!(
            p.text("{e:queue}"),
            "<tg-emoji emoji-id=\"222\">📋</tg-emoji>"
        );
        assert_eq!(p.text("{e:vault} {x} {"), "🏦 {x} {");
        assert_eq!(Look::plain().text("{e:alarm}"), "🚨");
    }

    #[test]
    fn text_buttons_become_plain_text_in_html() {
        assert_eq!(
            strip_text_buttons(
                "a <tg-button type=\"callback_data\" style=\"link\" data=\"a:o:1\">K7M2</tg-button> b"
            ),
            "a K7M2 b"
        );
    }

    #[test]
    fn button_token_becomes_an_icon() {
        let b = premium().button(InlineKeyboardButton::callback(
            "{m:queue} Разбор · 2",
            "a:q",
        ));
        assert_eq!(b.text, "Разбор · 2");
        assert_eq!(b.icon_custom_emoji_id.as_deref(), Some("222"));
        let b = Look::plain().button(InlineKeyboardButton::callback("{m:queue} Разбор", "a:q"));
        assert_eq!(b.text, "📋 Разбор");
        assert_eq!(b.icon_custom_emoji_id, None);
    }
}
