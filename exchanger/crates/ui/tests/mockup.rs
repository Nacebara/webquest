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
