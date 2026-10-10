//! Проверка разметки всех экранов (SPEC §6.1, CLAUDE.md правило 16): только разрешённые теги
//! и атрибуты, теги закрыты по порядку, `<`, `>` и `&` вне тегов — только сущностями, нет
//! вложенных цитат в обычном HTML, нет незаполненных плейсхолдеров. Плюс проверка клавиатур
//! (лимиты Bot API) и property-тест: чужой текст не ломает разметку.
#![allow(clippy::unwrap_used)]

mod common;

use botapi::InlineButtonAction;
use proptest::prelude::*;
use rust_decimal_macros::dec;
use ui::{Callback, Keyboard, Screen, UiError};

/// Теги обычного HTML Telegram и их атрибуты.
const PLAIN_TAGS: &[(&str, &[&str])] = &[
    ("b", &[]),
    ("i", &[]),
    ("u", &[]),
    ("s", &[]),
    ("code", &[]),
    ("pre", &[]),
    ("tg-spoiler", &[]),
    ("a", &["href"]),
    ("blockquote", &["expandable"]),
    ("tg-emoji", &["emoji-id"]),
    ("tg-time", &["unix", "format"]),
];

/// Дополнительно в rich-разметке Bot API 10.3 (lovec `RICH_TAGS` + SPEC §6.2).
const RICH_EXTRA_TAGS: &[(&str, &[&str])] = &[
    ("h3", &[]),
    ("h4", &[]),
    ("p", &[]),
    ("footer", &[]),
    ("details", &["open"]),
    ("summary", &[]),
    ("ul", &[]),
    ("li", &[]),
    ("table", &["bordered", "striped", "compact"]),
    ("caption", &[]),
    ("tr", &[]),
    ("th", &["align", "colspan"]),
    ("td", &["align", "colspan"]),
    ("br", &[]),
    ("hr", &[]),
];

const VOID_TAGS: &[&str] = &["br", "hr"];

fn allowed_attrs(name: &str, rich: bool) -> Option<&'static [&'static str]> {
    let extra: &[(&str, &[&str])] = if rich { RICH_EXTRA_TAGS } else { &[] };
    PLAIN_TAGS
        .iter()
        .chain(extra)
        .find(|(tag, _)| *tag == name)
        .map(|(_, attrs)| *attrs)
}

fn check_entities(text: &str) -> Result<(), String> {
    for (i, _) in text.match_indices('&') {
        let rest = &text[i..];
        if !["&amp;", "&lt;", "&gt;", "&quot;"]
            .iter()
            .any(|e| rest.starts_with(e))
        {
            return Err(format!("голый & в {text:?}"));
        }
    }
    Ok(())
}

/// Атрибуты тега: `name` или `name="value"`.
fn parse_attrs(tag: &str, mut s: &str, allowed: &[&str]) -> Result<(), String> {
    loop {
        s = s.trim_start();
        if s.is_empty() {
            return Ok(());
        }
        let name_len = s
            .find(|c: char| !(c.is_ascii_lowercase() || c == '-'))
            .unwrap_or(s.len());
        let name = &s[..name_len];
        if name.is_empty() {
            return Err(format!("мусор в атрибутах <{tag}>: {s:?}"));
        }
        if !allowed.contains(&name) {
            return Err(format!("атрибут {name} не разрешён у <{tag}>"));
        }
        s = &s[name_len..];
        let value = if let Some(after) = s.strip_prefix("=\"") {
            let end = after
                .find('"')
                .ok_or_else(|| format!("незакрытое значение {name}"))?;
            let value = &after[..end];
            check_entities(value)?;
            s = &after[end + 1..];
            Some(value)
        } else {
            None
        };
        match (tag, name, value) {
            ("tg-emoji", "emoji-id", Some(v))
            | ("tg-time", "unix", Some(v))
            | (_, "colspan", Some(v))
                if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => {}
            ("tg-time", "format", Some("t" | "dt")) => {}
            ("a", "href", Some(v))
                if v.starts_with("https://")
                    || v.starts_with("http://")
                    || v.starts_with("tg://") => {}
            (_, "align", Some("left" | "right" | "center")) => {}
            (_, "expandable" | "open" | "bordered" | "striped" | "compact", None) => {}
            _ => {
                return Err(format!(
                    "плохое значение атрибута {name}={value:?} у <{tag}>"
                ));
            }
        }
    }
}

/// Разметка правильная: теги из набора, закрыты по порядку, текст экранирован.
fn validate(html: &str, rich: bool) -> Result<(), String> {
    let mut stack: Vec<&str> = Vec::new();
    let mut rest = html;
    loop {
        let Some(start) = rest.find('<') else {
            if rest.contains('>') {
                return Err(format!("голый > в {rest:?}"));
            }
            check_entities(rest)?;
            break;
        };
        let text = &rest[..start];
        if text.contains('>') {
            return Err(format!("голый > в {text:?}"));
        }
        check_entities(text)?;
        let end = start
            + rest[start..]
                .find('>')
                .ok_or_else(|| format!("незакрытая < в {html}"))?;
        let inner = &rest[start + 1..end];
        rest = &rest[end + 1..];

        if let Some(name) = inner.strip_prefix('/') {
            match stack.pop() {
                Some(open) if open == name => continue,
                other => return Err(format!("</{name}> закрывает {other:?} в {html}")),
            }
        }
        let (inner, self_closing) = match inner.strip_suffix('/') {
            Some(inner) => (inner, true),
            None => (inner, false),
        };
        let (name, attrs) = inner.split_once(' ').unwrap_or((inner, ""));
        let allowed = allowed_attrs(name, rich).ok_or_else(|| {
            format!(
                "тег <{name}> не разрешён ({}) в {html}",
                if rich { "rich" } else { "html" }
            )
        })?;
        parse_attrs(name, attrs, allowed)?;
        if VOID_TAGS.contains(&name) {
            continue;
        }
        if self_closing {
            return Err(format!("<{name}/> не пустой тег"));
        }
        if !rich && name == "blockquote" && stack.contains(&"blockquote") {
            return Err(format!("вложенная цитата в {html}"));
        }
        stack.push(name);
    }
    if stack.is_empty() {
        Ok(())
    } else {
        Err(format!("не закрыты {stack:?} в {html}"))
    }
}

fn check_text_rules(name: &str, screen: &Screen, placeholders: bool) {
    let html = &screen.html;
    validate(html, false).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert!(!html.is_empty(), "{name}: пусто");
    assert!(!html.ends_with('\n'), "{name}: перевод строки в конце");
    assert!(
        !html.contains("\n</blockquote>"),
        "{name}: пустая строка в конце цитаты"
    );
    assert!(!html.contains('\r'), "{name}: \\r");
    assert!(html.chars().count() <= 4096, "{name}: длиннее 4096");
    if let Some(rich) = &screen.rich_html {
        validate(rich, true).unwrap_or_else(|e| panic!("{name} (rich): {e}"));
        assert!(
            !rich.contains('\n'),
            "{name}: перевод строки в rich вместо <br>"
        );
    }
    if placeholders {
        for text in std::iter::once(html).chain(screen.rich_html.as_ref()) {
            assert!(
                !text.contains('{') && !text.contains('}'),
                "{name}: незаполненный плейсхолдер в {text}"
            );
        }
    }
}

fn check_keyboard(name: &str, keyboard: &Keyboard) {
    match keyboard {
        Keyboard::Inline(markup) => {
            assert!(!markup.is_empty(), "{name}: пустая инлайн-клавиатура");
            for button in markup.buttons() {
                assert!(!button.text.trim().is_empty(), "{name}: кнопка без текста");
                assert!(button.text.chars().count() <= 64, "{name}: {}", button.text);
                if let Some(id) = &button.icon_custom_emoji_id {
                    assert!(id.bytes().all(|b| b.is_ascii_digit()), "{name}: icon {id}");
                }
                match &button.action {
                    InlineButtonAction::CallbackData(data) => {
                        assert!(data.len() <= 64, "{name}: callback_data {data}");
                        assert!(
                            Callback::parse(data).is_some(),
                            "{name}: {data} не разбирается"
                        );
                    }
                    InlineButtonAction::CopyText(text) => {
                        let n = text.chars().count();
                        assert!((1..=256).contains(&n), "{name}: copy_text {text}");
                    }
                    InlineButtonAction::Url(url) => {
                        assert!(url.starts_with("https://"), "{name}: url {url}");
                    }
                    InlineButtonAction::Disabled => {}
                    InlineButtonAction::Other => panic!("{name}: кнопка без действия"),
                }
            }
        }
        Keyboard::Reply(markup) => {
            for button in markup.keyboard.iter().flatten() {
                assert!(!button.text.trim().is_empty(), "{name}: кнопка без текста");
            }
            if let Some(placeholder) = &markup.input_field_placeholder {
                assert!((1..=64).contains(&placeholder.chars().count()));
            }
        }
        Keyboard::None | Keyboard::Remove => {}
    }
}

#[test]
fn every_screen_is_valid_in_both_modes() {
    for (mode, ui) in common::modes() {
        let gallery = common::gallery(&ui);
        assert!(gallery.len() > 70, "галерея неполная: {}", gallery.len());
        for (name, screen) in &gallery {
            let name = format!("{mode}/{name}");
            check_text_rules(&name, screen, true);
            check_keyboard(&name, &screen.keyboard);
            check_keyboard(&name, &ui.rich_keyboard(screen));
        }
    }
}

#[test]
fn money_flow_is_plain_html_reference_screens_have_fallback() {
    let ui = common::plain_ui();
    let rich = ["welcome", "terms", "history", "order_card", "privacy"];
    for (name, screen) in common::gallery(&ui) {
        let expect_rich = rich
            .iter()
            .any(|r| name == *r || name.starts_with(&format!("{r}_")))
            && name != "history_empty";
        assert_eq!(screen.is_rich(), expect_rich, "{name}");
    }
}

#[test]
fn plain_mode_has_no_custom_emoji_premium_mode_does() {
    for (name, screen) in common::gallery(&common::plain_ui()) {
        assert!(!screen.html.contains("tg-emoji"), "{name}");
        if let Some(markup) = screen.keyboard.inline() {
            assert!(
                markup.buttons().all(|b| b.icon_custom_emoji_id.is_none()),
                "{name}"
            );
        }
    }
    let premium = common::premium_ui();
    let screen = premium.order_checking(&common::order(domain::Direction::CbToXrCheck));
    assert!(
        screen.html.starts_with("<tg-emoji emoji-id=\"5368"),
        "{}",
        screen.html
    );
    let copy = &screen.keyboard.inline().unwrap().inline_keyboard[0][0];
    assert!(copy.icon_custom_emoji_id.is_some());
    assert!(
        !copy.text.contains('📋'),
        "при Premium эмодзи — иконкой, не в тексте"
    );
}

#[test]
fn validator_rejects_bad_markup() {
    let bad_plain = [
        "<script>x</script>",
        "a & b",
        "a > b",
        "<b>x",
        "<b><i>x</b></i>",
        "<blockquote>a<blockquote>b</blockquote></blockquote>",
        "<tg-emoji emoji-id=\"x1\">⏳</tg-emoji>",
        "<tg-time unix=\"1\" format=\"zz\">1</tg-time>",
        "<a href=\"javascript:alert(1)\">x</a>",
        "<b onclick=\"x\">x</b>",
        "<h3>rich only</h3>",
        "<br>",
    ];
    for html in bad_plain {
        assert!(validate(html, false).is_err(), "{html}");
    }
    assert!(validate("<h3>ok</h3><p>a<br>b</p><hr/>", true).is_ok());
    assert!(
        validate(
            "<table bordered compact><tr><td align=\"right\">1</td></tr></table>",
            true
        )
        .is_ok()
    );
    assert!(
        validate(
            "<b>a &amp; b &lt;c&gt;</b> <blockquote expandable>q</blockquote>",
            false
        )
        .is_ok()
    );
}

#[test]
fn dynamic_text_cannot_inject_markup_or_placeholders() {
    let ui = common::plain_ui();
    let screen = ui.invoice_quote(&common::quote(
        Some("{e_ok} {order} <b>жирный</b> &amp;"),
        false,
    ));
    assert!(
        screen
            .html
            .contains("<i>{e_ok} {order} &lt;b&gt;жирный&lt;/b&gt; &amp;amp;</i>"),
        "{}",
        screen.html
    );
    validate(&screen.html, false).unwrap();
}

fn hostile() -> impl Strategy<Value = String> {
    prop_oneof![
        any::<String>(),
        "[<>&\"'{}a-zа-яё /=\n😀-]{0,60}",
        Just("</blockquote><blockquote>".to_owned()),
        Just("<tg-emoji emoji-id=\"1\">x</tg-emoji>".to_owned()),
    ]
}

proptest! {
    #[test]
    fn foreign_text_never_breaks_markup(s in hostile()) {
        for (mode, ui) in common::modes() {
            let mut quote = common::quote(Some(&s), true);
            if let Some(fiat) = quote.fiat.as_mut() {
                fiat.currency = s.clone();
            }
            let mut rows = common::history_rows();
            rows[0].order = s.clone();
            let mut card = common::card(true);
            card.order.id = s.clone();
            card.check_url = Some(s.clone());
            let mut terms = common::terms();
            terms.bind_username = Some(s.clone());
            let order = ui::OrderRef { id: s.clone(), direction: domain::Direction::CbToXrCheck };
            let screens = [
                ui.invoice_quote(&quote),
                ui.error(&UiError::ForeignBot { bot: s.clone() }),
                ui.error(&UiError::Banned { until: None, reason: s.clone() }),
                ui.error(&UiError::Duplicate { order: s.clone() }),
                ui.error(&UiError::NotForUs { bind_username: Some(s.clone()) }),
                ui.history(&rows, None),
                ui.order_card(&card),
                ui.cancelled(Some(&s)),
                ui.support(Some(&s)),
                ui.terms(&terms),
                ui.order_done(&order, &common::breakdown(), &s, common::at(12, 0)),
                ui.manual_review(&order, Some(&common::usdt(dec!(1))), common::at(12, 0)),
            ];
            for screen in &screens {
                prop_assert!(validate(&screen.html, false).is_ok(), "{mode}: {:?} → {}", s, screen.html);
                if let Some(rich) = &screen.rich_html {
                    prop_assert!(validate(rich, true).is_ok(), "{mode}: {:?} → {}", s, rich);
                }
            }
        }
    }
}
