//! Главный экран пульта против утверждённого макета (`docs/mockups/template.html`,
//! «Главный экран · вариант А»: `DASH_OK`, `DASH_FIRE`, `DASH_CALM`). На данных макета
//! ([`ops::sample_dash`]) каждая строка карточки, каждый значок и каждая кнопка (подпись,
//! иконка, цвет) должны совпасть. Разметка макета — HTML страницы, у бота — rich Telegram,
//! поэтому текст сравнивается без пробелов и тегов.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use ops::{Look, Texts};

fn template() -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/mockups/template.html"
    );
    std::fs::read_to_string(path).expect("docs/mockups/template.html")
}

#[derive(Debug, Clone)]
enum Js {
    Str(String),
    Null,
    List(Vec<Js>),
}

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
        let end = rest.find('\'').unwrap();
        return (Js::Str(rest[..end].to_owned()), &rest[end + 1..]);
    }
    let end = src
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(src.len());
    (Js::Null, &src[end..])
}

/// Кнопка: подпись, ключ иконки, цвет.
type Button = (String, Option<String>, Option<String>);

/// Сообщение макета `const NAME = R(…)`: HTML и кнопки.
fn mock(name: &str) -> (String, Vec<Button>) {
    mock_after(&format!("const {name} = R(`"))
}

/// Первое сообщение бота (`B(` или `R(`) после метки `start`: HTML и кнопки.
fn mock_after(start: &str) -> (String, Vec<Button>) {
    let src = template();
    let at = src.find(start).expect(start);
    let rel = src[at..].find('`').expect("message text");
    let i = at + rel + 1;
    let end = src[i..].find('`').unwrap() + i;
    let html = src[i..end].to_owned();
    let after = src[end + 1..].trim_start().strip_prefix(',').unwrap();
    let (kb, _) = js(after);
    let mut buttons = Vec::new();
    let Js::List(rows) = kb else { panic!() };
    for row in rows {
        let Js::List(row) = row else { panic!() };
        for b in row {
            let Js::List(b) = b else { panic!() };
            let get = |i: usize| match b.get(i) {
                Some(Js::Str(s)) if !s.is_empty() => Some(s.clone()),
                _ => None,
            };
            buttons.push((
                get(0).unwrap().replace("&nbsp;", " "),
                get(1).map(|i| format!("mono/{i}")),
                get(2),
            ));
        }
    }
    (html, buttons)
}

/// Значки макета в тексте: `{key}` — цветной, `{-key}` — линейный.
fn mock_emoji(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find('{') {
        let Some(j) = rest[i..].find('}') else { break };
        let key = &rest[i + 1..i + j];
        let (kind, name) = match key.strip_prefix('-') {
            Some(n) => ("mono", n),
            None => ("color", key),
        };
        if !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
            out.push(format!("{kind}/{name}"));
        }
        rest = &rest[i + j + 1..];
    }
    out
}

/// Строки текста без тегов, значков и пробелов.
fn lines(html: &str) -> Vec<String> {
    // В исходнике макета перевод строки в обычном сообщении записан как `\n`.
    let mut s = html.replace("\\n", "\n");
    // Значки: токены макета и <tg-emoji>…</tg-emoji> бота.
    while let Some(i) = s.find("<tg-emoji") {
        let j = s[i..].find("</tg-emoji>").unwrap() + i + "</tg-emoji>".len();
        s.replace_range(i..j, "");
    }
    let mut cleaned = String::new();
    let mut rest = s.as_str();
    while let Some(i) = rest.find('{') {
        cleaned.push_str(&rest[..i]);
        match rest[i..].find('}') {
            Some(j)
                if rest[i + 1..i + j]
                    .trim_start_matches('-')
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'_') =>
            {
                rest = &rest[i + j + 1..];
            }
            _ => {
                cleaned.push('{');
                rest = &rest[i + 1..];
            }
        }
    }
    cleaned.push_str(rest);
    for tag in [
        "</div>",
        "</p>",
        "</h3>",
        "</tr>",
        "<hr>",
        "<hr/>",
        "</footer>",
        "</table>",
    ] {
        cleaned = cleaned.replace(tag, &format!("\n{tag}"));
    }
    let mut text = String::new();
    let mut in_tag = false;
    for c in cleaned.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    text.replace("&nbsp;", "")
        .replace("&amp;", "&")
        .lines()
        .map(|l| l.chars().filter(|c| !c.is_whitespace()).collect::<String>())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Вид, где id каждого значка — его номер: по id виден ключ.
fn numbered(keys: &[String]) -> (Look, BTreeMap<String, String>) {
    let mut packs = ui::EmojiPacks::default();
    let mut back = BTreeMap::new();
    for (n, key) in keys.iter().enumerate() {
        let id = format!("{}", 7_000_000 + n);
        packs.animated.insert(key.clone(), id.clone());
        back.insert(id, key.clone());
    }
    let settings = ui::EmojiSettings {
        style: ui::EmojiStyle::Animated,
        packs,
        custom: BTreeMap::new(),
    };
    (
        Look::new(ui::EmojiSet::from_settings(true, &settings).unwrap()),
        back,
    )
}

#[test]
fn dash_says_and_shows_what_the_mockup_does() {
    let texts = Texts::builtin().unwrap();
    let mut problems = Vec::new();
    let states = ops::sample_dash();
    let names = ["DASH_OK", "DASH_FIRE", "DASH_CALM"];
    for ((title, data), name) in states.iter().zip(names) {
        let (html, buttons) = mock(name);
        let mut keys = mock_emoji(&html);
        keys.extend(buttons.iter().filter_map(|b| b.1.clone()));
        for k in [
            "cb", "xr", "dot_ok", "dot_wait", "dot_act", "live", "alarm", "kill", "fast",
        ] {
            keys.push(format!("color/{k}"));
        }
        keys.sort();
        keys.dedup();
        let (look, back) = numbered(&keys);
        let screen = look.screen(&data.screen(&texts));
        let rich = screen.rich.clone().unwrap();
        // Текст.
        let ours = lines(&rich).join("|");
        for line in lines(&html) {
            if !ours.contains(&line) {
                problems.push(format!("«{title}»: в макете «{line}»\n  у бота: {ours}"));
            }
        }
        // Значки в тексте.
        let mut found = Vec::new();
        let mut rest = rich.as_str();
        while let Some(i) = rest.find("emoji-id=\"") {
            rest = &rest[i + 10..];
            let end = rest.find('"').unwrap();
            found.push(back.get(&rest[..end]).cloned().unwrap_or_default());
        }
        for e in mock_emoji(&html) {
            if !found.contains(&e) {
                problems.push(format!("«{title}»: нет значка {e} (у бота {found:?})"));
            }
        }
        // Кнопки: подпись, иконка, цвет — по порядку.
        let ours: Vec<Button> = screen
            .keyboard
            .iter()
            .flatten()
            .map(|b| {
                (
                    b.text.clone(),
                    b.icon_custom_emoji_id
                        .as_ref()
                        .and_then(|id| back.get(id).cloned()),
                    b.style.map(|s| {
                        match s {
                            botapi::ButtonStyle::Primary => "primary",
                            botapi::ButtonStyle::Success => "success",
                            botapi::ButtonStyle::Danger => "danger",
                        }
                        .to_owned()
                    }),
                )
            })
            .collect();
        if ours != buttons {
            problems.push(format!(
                "«{title}»: кнопки\n  макет: {buttons:?}\n  бот:   {ours:?}"
            ));
        }
        // Раскладка: по две в ряд, как в макете.
        if screen.keyboard.iter().any(|r| r.len() > 2) {
            problems.push(format!("«{title}»: больше двух кнопок в ряду"));
        }
    }
    assert!(
        problems.is_empty(),
        "расхождения с макетом:\n{}",
        problems.join("\n")
    );
}

#[test]
fn menu_is_the_mockup() {
    let texts = Texts::builtin().unwrap();
    let (html, buttons) = mock_after("t: 'Меню пульта'");
    let mut keys: Vec<String> = buttons.iter().filter_map(|b| b.1.clone()).collect();
    keys.sort();
    keys.dedup();
    let (look, back) = numbered(&keys);
    // Данные макета: владелец, на разборе 2, обращение 1.
    let counts = ops::MenuCounts {
        review: 2,
        inbox: 1,
        ..Default::default()
    };
    let screen = look.screen(&ops::menu_screen(
        &texts,
        storage::staff::Role::Owner,
        &counts,
    ));
    let ours = lines(&screen.text).join("|");
    for line in lines(&html) {
        assert!(ours.contains(&line), "в макете «{line}», у бота: {ours}");
    }
    let ours: Vec<Button> = screen
        .keyboard
        .iter()
        .flatten()
        .map(|b| {
            (
                b.text.clone(),
                b.icon_custom_emoji_id
                    .as_ref()
                    .and_then(|id| back.get(id).cloned()),
                b.style.map(|s| {
                    match s {
                        botapi::ButtonStyle::Primary => "primary",
                        botapi::ButtonStyle::Success => "success",
                        botapi::ButtonStyle::Danger => "danger",
                    }
                    .to_owned()
                }),
            )
        })
        .collect();
    assert_eq!(ours, buttons, "кнопки меню");
    let rows: Vec<usize> = screen.keyboard.iter().map(Vec::len).collect();
    assert_eq!(
        rows,
        vec![2, 2, 2, 2, 2, 2, 2, 1],
        "по два в ряд, «Стоп-кран» один внизу"
    );
}

/// Кнопки экрана как в макете: подпись, ключ иконки, цвет.
fn our_buttons(screen: &ops::Screen, back: &BTreeMap<String, String>) -> Vec<Button> {
    screen
        .keyboard
        .iter()
        .flatten()
        .map(|b| {
            (
                b.text.clone(),
                b.icon_custom_emoji_id
                    .as_ref()
                    .and_then(|id| back.get(id).cloned()),
                b.style.map(|s| {
                    match s {
                        botapi::ButtonStyle::Primary => "primary",
                        botapi::ButtonStyle::Success => "success",
                        botapi::ButtonStyle::Danger => "danger",
                    }
                    .to_owned()
                }),
            )
        })
        .collect()
}

/// Экран против сообщения макета: строки, значки в тексте, кнопки.
fn check(
    title: &str,
    html: &str,
    buttons: &[Button],
    screen: &ops::Screen,
    back: &BTreeMap<String, String>,
) {
    let body = screen.rich.clone().unwrap_or_else(|| screen.text.clone());
    let ours = lines(&body).join("|");
    for line in lines(html) {
        assert!(
            ours.contains(&line),
            "«{title}»: в макете «{line}», у бота: {ours}"
        );
    }
    let mut found = Vec::new();
    let mut rest = body.as_str();
    while let Some(i) = rest.find("emoji-id=\"") {
        rest = &rest[i + 10..];
        let end = rest.find('"').unwrap();
        found.push(back.get(&rest[..end]).cloned().unwrap_or_default());
    }
    for e in mock_emoji(html) {
        assert!(
            found.contains(&e),
            "«{title}»: нет значка {e} (у бота {found:?})"
        );
    }
    assert_eq!(our_buttons(screen, back), buttons, "«{title}»: кнопки");
}

fn keys_of(html: &str, buttons: &[Button]) -> Vec<String> {
    let mut keys = mock_emoji(html);
    keys.extend(buttons.iter().filter_map(|b| b.1.clone()));
    keys.sort();
    keys.dedup();
    keys
}

#[test]
fn reserves_and_shortfall_are_the_mockup() {
    let texts = Texts::builtin().unwrap();
    let (html, buttons) = mock_after("t: 'Резервы и сверка'");
    let (look, back) = numbered(&keys_of(&html, &buttons));
    let screen = look.screen(&ops::sample_reserves(&texts).screen(&texts));
    check("Резервы и сверка", &html, &buttons, &screen, &back);

    let (html, buttons) = mock_after("t: 'Недостача'");
    let (look, back) = numbered(&keys_of(&html, &buttons));
    let payload = serde_json::json!({
        "platform": "xrocket",
        "expected": "640",
        "actual": "628",
        "diff": "-12",
        "paused": true,
        "account": "ub-1",
    });
    let screen = look.screen(&ops::mismatch_screen(&texts, &payload, 1));
    check("Недостача", &html, &buttons, &screen, &back);
}
