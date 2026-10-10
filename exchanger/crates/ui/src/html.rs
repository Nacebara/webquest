//! Экранирование и фрагменты разметки сразу в двух видах — rich-HTML (Bot API 10.3) и обычный
//! HTML Telegram. Всё, что пришло извне (описание счёта, имя бота из ссылки, причина бана,
//! номер заявки, URL), попадает в разметку только через [`Frag::text`] / [`Frag::code`] /
//! [`esc`]. Шаблоны из `ru.toml` — доверенные: в них может быть разметка.

use std::borrow::Cow;

use crate::emoji::{Emoji, EmojiSet};

/// Экранирование `& < > "` (как lovec `format.rs:14-34`; кавычка — для `href="…"`).
pub fn esc(s: &str) -> Cow<'_, str> {
    if !s.bytes().any(|b| matches!(b, b'&' | b'<' | b'>' | b'"')) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 16);
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    Cow::Owned(out)
}

/// Обрезать по символам с «…».
pub fn clip(s: &str, max_chars: usize) -> Cow<'_, str> {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => Cow::Owned(format!("{}…", &s[..idx])),
        None => Cow::Borrowed(s),
    }
}

/// Фрагмент разметки в двух видах. В rich-виде перевод строки — `<br>`, в обычном — `\n`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frag {
    rich: String,
    plain: String,
}

impl Frag {
    pub fn new() -> Self {
        Self::default()
    }

    /// Внешний текст: экранируется.
    pub fn text(s: &str) -> Self {
        let escaped = esc(s);
        Self {
            rich: escaped.replace('\n', "<br>"),
            plain: escaped.into_owned(),
        }
    }

    /// Внешний текст моноширинным (суммы, номера, ссылки — SPEC §6.1).
    pub fn code(s: &str) -> Self {
        let escaped = esc(&s.replace('\n', " ")).into_owned();
        let html = format!("<code>{escaped}</code>");
        Self {
            rich: html.clone(),
            plain: html,
        }
    }

    /// Доверенная разметка (шаблон из `ru.toml`, литерал в коде).
    pub(crate) fn trusted(markup: &str) -> Self {
        Self {
            rich: markup.replace('\n', "<br>"),
            plain: markup.to_owned(),
        }
    }

    /// Разная доверенная разметка для двух видов.
    pub(crate) fn dual(rich: String, plain: String) -> Self {
        Self { rich, plain }
    }

    pub fn bold(inner: Frag) -> Self {
        Self {
            rich: format!("<b>{}</b>", inner.rich),
            plain: format!("<b>{}</b>", inner.plain),
        }
    }

    pub fn italic(inner: Frag) -> Self {
        Self {
            rich: format!("<i>{}</i>", inner.rich),
            plain: format!("<i>{}</i>", inner.plain),
        }
    }

    /// Ссылка; URL и текст экранируются.
    pub fn link(url: &str, text: &str) -> Self {
        let html = format!("<a href=\"{}\">{}</a>", esc(url), esc(text));
        Self {
            rich: html.clone(),
            plain: html,
        }
    }

    pub fn push(&mut self, other: &Frag) -> &mut Self {
        self.rich.push_str(&other.rich);
        self.plain.push_str(&other.plain);
        self
    }

    pub fn push_text(&mut self, s: &str) -> &mut Self {
        self.push(&Frag::text(s))
    }

    pub fn is_empty(&self) -> bool {
        self.plain.is_empty() && self.rich.is_empty()
    }

    pub fn rich(&self) -> &str {
        &self.rich
    }

    pub fn plain(&self) -> &str {
        &self.plain
    }

    pub fn into_plain(self) -> String {
        self.plain
    }

    /// Склеить фрагменты через разделитель (доверенный).
    pub fn join(parts: &[Frag], separator: &str) -> Frag {
        let sep = Frag::trusted(separator);
        let mut out = Frag::new();
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                out.push(&sep);
            }
            out.push(part);
        }
        out
    }
}

/// Подставить значения в шаблон. `{e_*}` без явного значения раскрывается эмодзи из набора;
/// незнакомый плейсхолдер остаётся как есть (тесты ловят `{` в выводе).
pub(crate) fn fill(template: &str, args: &[(&str, &Frag)], emoji: &EmojiSet) -> Frag {
    let mut out = Frag::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push(&Frag::trusted(&rest[..open]));
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push(&Frag::trusted(&rest[open..]));
            return out;
        };
        let name = &after[..close];
        if let Some((_, value)) = args.iter().find(|(key, _)| *key == name) {
            out.push(value);
        } else if let Some(role) = name.strip_prefix("e_").and_then(Emoji::from_key) {
            out.push(&emoji.frag(role));
        } else {
            out.push(&Frag::trusted(&rest[open..open + close + 2]));
        }
        rest = &after[close + 1..];
    }
    out.push(&Frag::trusted(rest));
    out
}

/// То же для текста кнопок: обычный текст без HTML, значения не экранируются.
pub(crate) fn fill_text(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let name = &after[..close];
        match args.iter().find(|(key, _)| *key == name) {
            Some((_, value)) => out.push_str(value),
            None => out.push_str(&rest[open..open + close + 2]),
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_matches_lovec() {
        assert_eq!(esc("a<b>&\"c\""), "a&lt;b&gt;&amp;&quot;c&quot;");
        assert!(matches!(esc("plain"), Cow::Borrowed(_)));
        assert_eq!(esc("ёжик & 🚀"), "ёжик &amp; 🚀");
    }

    #[test]
    fn clip_counts_chars() {
        assert_eq!(clip("абвгд", 3), "абв…");
        assert_eq!(clip("абв", 3), "абв");
    }

    #[test]
    fn fill_escapes_only_values_and_expands_emoji() {
        let emoji = EmojiSet::plain();
        let order = Frag::code("A<1>");
        let out = fill(
            "{e_ok} <b>Заявка</b> {order}\n{unknown}",
            &[("order", &order)],
            &emoji,
        );
        assert_eq!(
            out.plain(),
            "✅ <b>Заявка</b> <code>A&lt;1&gt;</code>\n{unknown}"
        );
        assert_eq!(
            out.rich(),
            "✅ <b>Заявка</b> <code>A&lt;1&gt;</code><br>{unknown}"
        );
    }

    #[test]
    fn fill_text_is_not_html() {
        assert_eq!(
            fill_text("Пришлю чек {wallet}", &[("wallet", "x<R>")]),
            "Пришлю чек x<R>"
        );
    }
}
