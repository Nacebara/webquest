//! Сверка экранов клиента с утверждённым макетом (`docs/mockups/template.html`, вкладка
//! «Клиент», вариант B1). Каждая строка каждого сообщения бота из макета должна быть в экране
//! бота с тем же названием (`ui::preview`, те же данные, что в макете). Эмодзи, разметка и
//! пояс времени («МСК») не сравниваются — только текст.
//!
//! Не совпало — значит, экран разошёлся с макетом: исправить экран, а не тест. Отступления,
//! согласованные или вынужденные, — в [`KNOWN`] с причиной.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use ui::{EmojiSet, Ui, UiConfig};

/// (экран, строка макета, почему её нет в боте).
const KNOWN: &[(&str, &str, &str)] = &[
    (
        "Карточка заявки",
        "Чек не забран",
        "бот пока не знает, забран ли чек: пишет «Чек: в кнопке ниже»",
    ),
    (
        "Счёт оплачен",
        "Счёт IV…7Q · 14:13",
        "код счёта в заявке не хранится: «Заявка K7M2Q9XR4T · 14:13»",
    ),
    (
        "Лимит и пауза",
        "Счета xRocket на паузе",
        "в отказе нет кошелька счёта: «Оплата счетов на паузе»",
    ),
    (
        "Лимит и пауза",
        "Обычно до часа. Обмен чеков работает.",
        "срок паузы бот не знает: «Пока можно обменять чек.»",
    ),
    (
        "Поддержка",
        "Оператор Анна: деньги у нас, проверяем. Чек будет через 5 минут.",
        "это ответ оператора — его присылает пульт (тест ops_bot)",
    ),
];

fn template() -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/mockups/template.html"
    );
    std::fs::read_to_string(path).expect("docs/mockups/template.html")
}

/// Тексты сообщений бота (`B(` и `R(`) в куске исходника макета.
fn bot_messages(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(i) = rest.find(['B', 'R']) {
        let tail = &rest[i..];
        if (tail.starts_with("B(`") || tail.starts_with("R(`"))
            && !rest[..i].ends_with(|c: char| c.is_alphanumeric())
        {
            let body = &tail[3..];
            let end = body.find('`').expect("closing backtick");
            out.push(body[..end].to_owned());
            rest = &body[end + 1..];
        } else {
            rest = &tail[1..];
        }
    }
    out
}

/// Экран макета → сообщения бота. `PROG` и вариант B1 разворачиваются.
fn mockup() -> BTreeMap<String, Vec<String>> {
    let src = template();
    let block = |start: &str, end: &str| {
        let i = src.find(start).expect(start);
        let j = src[i..].find(end).expect(end) + i;
        src[i..j].to_owned()
    };
    let prog = block("const PROG = {", "\n};");
    let b1 = block("id: 'B1'", "\n  },\n  {");
    let entry = |src: &str, key: &str| {
        let i = src.find(&format!("'{key}':")).expect(key);
        let line_end = src[i..].find('\n').map_or(src.len(), |j| i + j);
        bot_messages(&src[i..line_end])
    };
    let flow = block("const FLOW = [", "\n];");
    let mut out = BTreeMap::new();
    for item in flow.split("\n  { t: '").skip(1) {
        let title = item[..item.find('\'').unwrap()].to_owned();
        let messages = match title.as_str() {
            "Проверяю" => entry(&prog, "Проверяю"),
            "Отправляю" => entry(&prog, "Отправляю"),
            "Дольше обычного" => entry(&prog, "Долго"),
            "Готово" => entry(&b1, "Готово"),
            "Детали" => entry(&b1, "Детали"),
            _ => bot_messages(item),
        };
        out.insert(title, messages);
    }
    out
}

/// Текст без разметки, эмодзи и пояса: строки по блокам.
fn lines(html: &str) -> Vec<String> {
    // В исходнике макета перевод строки внутри шаблона JS записан как `\n`.
    let mut s = html
        .replace("${ORD}", ui::preview::ORDER)
        .replace("\\n", "\n");
    // {key} и {-key} — эмодзи макета.
    let mut cleaned = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            let mut inner = String::new();
            let mut closed = false;
            for d in chars.by_ref() {
                if d == '}' {
                    closed = true;
                    break;
                }
                inner.push(d);
            }
            let is_key = closed
                && inner
                    .trim_start_matches('-')
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_');
            if !is_key {
                cleaned.push('{');
                cleaned.push_str(&inner);
                if closed {
                    cleaned.push('}');
                }
            }
        } else {
            cleaned.push(c);
        }
    }
    s = cleaned;
    for tag in [
        "<br>",
        "</p>",
        "</h3>",
        "</tr>",
        "<blockquote",
        "</blockquote>",
        "<bq>",
        "</bq>",
        "<bqx>",
        "</bqx>",
        "<footer>",
        "</footer>",
        "</li>",
        "</summary>",
        "</details>",
    ] {
        s = s.replace(tag, &format!("\n{tag}"));
    }
    s = s.replace("</td>", " </td>").replace("</th>", " </th>");
    let mut text = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    let text = text
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
        .replace('\u{a0}', " ")
        .replace(" МСК", "");
    text.lines()
        .map(|l| {
            let kept: String = l
                .chars()
                .filter(|c| {
                    c.is_alphanumeric()
                        || c.is_whitespace()
                        || "·.,:;!?—–-()«»%$/+<>=\"'@_#…→⇄×&".contains(*c)
                })
                .collect();
            kept.split_whitespace().collect::<Vec<_>>().join(" ")
        })
        .filter(|l| !l.is_empty())
        .collect()
}

#[test]
fn every_client_screen_says_what_the_mockup_says() {
    let mockup = mockup();
    let ui = Ui::builtin(EmojiSet::plain(), UiConfig::default()).unwrap();
    let preview = ui::preview::client(&ui);
    let mut problems = Vec::new();
    for screen in &preview {
        let Some(expected) = mockup.get(screen.title) else {
            problems.push(format!("«{}»: нет такого экрана в макете", screen.title));
            continue;
        };
        let ours = screen
            .messages
            .iter()
            .flat_map(|m| lines(m.rich_html.as_deref().unwrap_or(&m.html)))
            .collect::<Vec<_>>()
            .join(" ");
        for line in expected.iter().flat_map(|m| lines(m)) {
            let known = KNOWN
                .iter()
                .any(|(title, l, _)| *title == screen.title && *l == line);
            if !ours.contains(&line) && !known {
                problems.push(format!(
                    "«{}»: в макете «{line}»\n  у бота: {ours}",
                    screen.title
                ));
            }
        }
    }
    for title in mockup.keys() {
        if !preview.iter().any(|s| s.title == title) {
            problems.push(format!(
                "«{title}»: экран макета не показан в предпросмотре"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "расхождения с макетом:\n{}",
        problems.join("\n")
    );
}

#[test]
fn known_deviations_are_still_needed() {
    // Отступление исправили — убрать его из KNOWN, чтобы тест снова сторожил эту строку.
    let ui = Ui::builtin(EmojiSet::plain(), UiConfig::default()).unwrap();
    for (title, line, why) in KNOWN {
        let screen = ui::preview::client(&ui)
            .into_iter()
            .find(|s| s.title == *title)
            .unwrap();
        let ours = screen
            .messages
            .iter()
            .flat_map(|m| lines(m.rich_html.as_deref().unwrap_or(&m.html)))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            !ours.contains(line),
            "«{title}»: «{line}» уже совпадает ({why})"
        );
    }
}

// ---- эмодзи и кнопки ---------------------------------------------------------------------

/// Значение литерала JS в клавиатуре макета: строка, `null`, имя (`ORD`) или массив.
#[derive(Debug, Clone)]
enum Js {
    Str(String),
    Null,
    Name(String),
    List(Vec<Js>),
}

impl Js {
    fn str(&self) -> Option<&str> {
        match self {
            Js::Str(s) | Js::Name(s) => Some(s),
            _ => None,
        }
    }

    fn list(&self) -> &[Js] {
        match self {
            Js::List(l) => l,
            _ => &[],
        }
    }
}

/// Разобрать литерал JS с начала `src`; вернуть значение и остаток.
fn js(src: &str) -> (Js, &str) {
    let src = src.trim_start();
    if let Some(rest) = src.strip_prefix('[') {
        let mut items = Vec::new();
        let mut rest = rest.trim_start();
        loop {
            if let Some(r) = rest.strip_prefix(']') {
                return (Js::List(items), r);
            }
            let (item, r) = js(rest);
            items.push(item);
            rest = r.trim_start();
            rest = rest.strip_prefix(',').unwrap_or(rest).trim_start();
        }
    }
    if let Some(rest) = src.strip_prefix('\'') {
        let mut out = String::new();
        let mut chars = rest.char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                '\\' => {
                    if let Some((_, d)) = chars.next() {
                        out.push(d);
                    }
                }
                '\'' => return (Js::Str(out), &rest[i + 1..]),
                c => out.push(c),
            }
        }
        panic!("unterminated string");
    }
    let end = src
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .unwrap_or(src.len());
    let word = &src[..end];
    let value = if word == "null" {
        Js::Null
    } else {
        Js::Name(word.to_owned())
    };
    (value, &src[end..])
}

/// Кнопка макета: иконка (`mono/…`) и цвет.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MockButton {
    label: String,
    icon: Option<String>,
    style: Option<String>,
}

/// Сообщение бота из макета: эмодзи в тексте (`color/…`, `mono/…`) и кнопки по рядам.
#[derive(Debug, Clone)]
struct MockMessage {
    emoji: Vec<String>,
    buttons: Vec<Vec<MockButton>>,
}

fn mock_messages(src: &str) -> Vec<MockMessage> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(i) = rest.find(['B', 'R']) {
        let tail = &rest[i..];
        if !((tail.starts_with("B(`") || tail.starts_with("R(`"))
            && !rest[..i].ends_with(|c: char| c.is_alphanumeric()))
        {
            rest = &tail[1..];
            continue;
        }
        let body = &tail[3..];
        let end = body.find('`').expect("closing backtick");
        let text = &body[..end];
        let mut emoji = Vec::new();
        let mut t = text;
        while let Some(open) = t.find('{') {
            let Some(close) = t[open..].find('}') else {
                break;
            };
            let key = &t[open + 1..open + close];
            let (mono, name) = match key.strip_prefix('-') {
                Some(name) => (true, name),
                None => (false, key),
            };
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                emoji.push(format!("{}/{name}", if mono { "mono" } else { "color" }));
            }
            t = &t[open + close + 1..];
        }
        let after = &body[end + 1..];
        let mut buttons = Vec::new();
        if let Some(kb) = after.trim_start().strip_prefix(',') {
            let (value, _) = js(kb);
            for row in value.list() {
                let row: Vec<MockButton> = row
                    .list()
                    .iter()
                    .map(|b| {
                        let b = b.list();
                        let get = |i: usize| {
                            b.get(i)
                                .and_then(Js::str)
                                .filter(|s| !s.is_empty())
                                .map(str::to_owned)
                        };
                        MockButton {
                            label: get(0).unwrap_or_default(),
                            icon: get(1).map(|i| format!("mono/{i}")),
                            style: get(2),
                        }
                    })
                    .collect();
                buttons.push(row);
            }
        }
        out.push(MockMessage { emoji, buttons });
        rest = after;
    }
    out
}

/// Экран макета → сообщения бота с эмодзи и кнопками (как [`mockup`]).
fn mockup_full() -> BTreeMap<String, Vec<MockMessage>> {
    let src = template();
    let block = |start: &str, end: &str| {
        let i = src.find(start).expect(start);
        let j = src[i..].find(end).expect(end) + i;
        src[i..j].to_owned()
    };
    let prog = block("const PROG = {", "\n};");
    let b1 = block("id: 'B1'", "\n  },\n  {");
    let entry = |src: &str, key: &str| {
        let i = src.find(&format!("'{key}':")).expect(key);
        let line_end = src[i..].find('\n').map_or(src.len(), |j| i + j);
        mock_messages(&src[i..line_end])
    };
    let flow = block("const FLOW = [", "\n];");
    let mut out = BTreeMap::new();
    for item in flow.split("\n  { t: '").skip(1) {
        let title = item[..item.find('\'').unwrap()].to_owned();
        let messages = match title.as_str() {
            "Проверяю" => entry(&prog, "Проверяю"),
            "Отправляю" => entry(&prog, "Отправляю"),
            "Дольше обычного" => entry(&prog, "Долго"),
            "Готово" => entry(&b1, "Готово"),
            "Детали" => entry(&b1, "Детали"),
            _ => mock_messages(item),
        };
        out.insert(title, messages);
    }
    out
}

/// Набор эмодзи, где id каждого значка — его номер в списке ключей: по id виден ключ.
fn numbered_emoji() -> (EmojiSet, BTreeMap<String, String>) {
    let src = template();
    let mut keys = std::collections::BTreeSet::new();
    let mut rest = src.as_str();
    while let Some(i) = rest.find("data-k=\"") {
        rest = &rest[i + 8..];
        let end = rest.find('"').unwrap();
        keys.insert(rest[..end].to_owned());
    }
    for kind in ["color", "mono"] {
        for role in ui::Emoji::ALL {
            for icon in [role.text_icon(), role.button_icon()] {
                if icon.starts_with(kind) {
                    keys.insert(icon.to_owned());
                }
            }
        }
    }
    let mut packs = BTreeMap::new();
    let mut back = BTreeMap::new();
    for (n, key) in keys.iter().enumerate() {
        let id = format!("{}", 5_000_000 + n);
        packs.insert(key.clone(), id.clone());
        back.insert(id, key.clone());
    }
    let settings = ui::EmojiSettings {
        style: ui::EmojiStyle::Animated,
        packs: ui::EmojiPacks {
            animated: packs,
            still: BTreeMap::new(),
        },
        custom: BTreeMap::new(),
    };
    (EmojiSet::from_settings(true, &settings).unwrap(), back)
}

/// Кнопка бота: текст, иконка (ключ значка), цвет.
type OurButton = (String, Option<String>, Option<String>);

fn style_name(s: Option<botapi::ButtonStyle>) -> Option<String> {
    s.map(|s| {
        match s {
            botapi::ButtonStyle::Primary => "primary",
            botapi::ButtonStyle::Success => "success",
            botapi::ButtonStyle::Danger => "danger",
        }
        .to_owned()
    })
}

#[test]
fn every_client_screen_has_the_mockup_emoji_and_buttons() {
    let mockup = mockup_full();
    let (emoji, back) = numbered_emoji();
    let ui = Ui::builtin(emoji, UiConfig::default()).unwrap();
    let key = |id: &str| back.get(id).cloned().unwrap_or_else(|| format!("?{id}"));
    let mut problems = Vec::new();
    for screen in ui::preview::client(&ui) {
        let Some(expected) = mockup.get(screen.title) else {
            continue;
        };
        for (n, (mock, ours)) in expected.iter().zip(&screen.messages).enumerate() {
            let html = ours.rich_html.as_deref().unwrap_or(&ours.html);
            let mut found = Vec::new();
            let mut rest = html;
            while let Some(i) = rest.find("emoji-id=\"") {
                rest = &rest[i + 10..];
                let end = rest.find('"').unwrap();
                found.push(key(&rest[..end]));
            }
            for e in &mock.emoji {
                if !found.contains(e) {
                    problems.push(format!(
                        "«{}» #{n}: в тексте нет {e} (у бота: {found:?})",
                        screen.title
                    ));
                }
            }
            let rows: Vec<Vec<OurButton>> = match &ours.keyboard {
                ui::Keyboard::Inline(m) => m
                    .inline_keyboard
                    .iter()
                    .map(|r| {
                        r.iter()
                            .map(|b| {
                                (
                                    b.text.clone(),
                                    b.icon_custom_emoji_id.as_deref().map(key),
                                    style_name(b.style),
                                )
                            })
                            .collect()
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let mut flat_ours: Vec<_> = rows.iter().flatten().collect();
            if screen.title == "История" {
                // В макете — кнопки двух заявок из четырёх и «Ещё»; у бота кнопка на каждую
                // строку таблицы (тап по любой — карточка). Сверяем первые две и «Ещё».
                let more = flat_ours.pop();
                flat_ours.truncate(2);
                flat_ours.extend(more);
            }
            let flat_mock: Vec<_> = mock.buttons.iter().flatten().collect();
            if flat_ours.len() != flat_mock.len() {
                problems.push(format!(
                    "«{}» #{n}: кнопок {} вместо {} (у бота: {rows:?})",
                    screen.title,
                    flat_ours.len(),
                    flat_mock.len()
                ));
            }
            for (m, o) in flat_mock.iter().zip(&flat_ours) {
                if m.icon != o.1 || m.style != o.2 {
                    problems.push(format!(
                        "«{}» #{n}: кнопка «{}» — в макете {:?} {:?}, у бота «{}» {:?} {:?}",
                        screen.title, m.label, m.icon, m.style, o.0, o.1, o.2
                    ));
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "эмодзи и кнопки:\n{}",
        problems.join("\n")
    );
}
