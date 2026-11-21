//! Сверка экранов бота с реестром принятых макетов (`docs/mockups/accepted.json`, собирает
//! `tools/mockups/accepted.py`) по учётной книге `docs/mockups/accepted-ledger.json`.
//!
//! Каждый принятый экран в книге учтён ровно одним из статусов:
//! * `match` — сверен: экран бота (`screens`) говорит то же, что макет; проверяет тест;
//! * `todo` — ещё не по макету (`kind`, `why`; строка в `docs/audit-prr/SCREENS-21-11.md`);
//! * `superseded` — заменён более поздним решением владельца (`by`, `why`);
//! * `later` — виджет: ждёт, пока Telegram включит виджеты.
//!
//! Сравнение: каждая строка макета есть в тексте бота (числа и номера — «#»: данные макета и
//! образцов разные), главное сообщение — того же вида (карточка / обычное), каждая кнопка макета
//! есть в боте с тем же цветом и, если в макете значок, — со значком.
//!
//! Подключается в тестах `ui` и `ops` через `#[path]`.
#![allow(dead_code)]

use std::collections::BTreeMap;

use serde_json::Value;

/// Экран бота для сверки: текст (rich или HTML) и кнопки.
#[derive(Debug, Clone, Default)]
pub struct Code {
    pub rich: Option<String>,
    pub html: String,
    pub buttons: Vec<Button>,
}

#[derive(Debug, Clone)]
pub struct Button {
    pub label: String,
    pub style: Option<String>,
    pub icon: bool,
}

fn repo(path: &str) -> String {
    format!("{}/../../{path}", env!("CARGO_MANIFEST_DIR"))
}

pub fn registry() -> Vec<Value> {
    let text = std::fs::read_to_string(repo("docs/mockups/accepted.json"))
        .expect("docs/mockups/accepted.json — python3 -I tools/mockups/accepted.py");
    serde_json::from_str::<Vec<Value>>(&text).expect("accepted.json")
}

pub fn ledger() -> BTreeMap<String, Value> {
    let text = std::fs::read_to_string(repo("docs/mockups/accepted-ledger.json"))
        .expect("docs/mockups/accepted-ledger.json");
    serde_json::from_str(&text).expect("accepted-ledger.json")
}

fn is_emoji(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x231A..=0x23FF
        | 0x25B6 | 0x25C0 | 0x21A9 | 0xFE0F | 0x2934 | 0x2935)
}

/// Слово с цифрой — «#»; подряд идущие «#» — один.
fn mask(line: &str) -> String {
    let words: Vec<String> = line
        .split_whitespace()
        .map(|w| {
            if w.chars().any(|c| c.is_ascii_digit()) {
                "#".to_owned()
            } else {
                w.to_owned()
            }
        })
        .collect();
    let mut out: Vec<String> = Vec::new();
    for w in words {
        if w == "#" && out.last().is_some_and(|l| l == "#") {
            continue;
        }
        out.push(w);
    }
    out.join(" ")
}

fn unescape(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
        .replace('\u{a0}', " ")
}

/// Убрать токены значков макета `{key}` / `{-key}` и пульта `{e:key}` / `{m:key}`.
fn strip_tokens(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        match tail.find('}') {
            Some(j)
                if tail[..j]
                    .trim_start_matches('-')
                    .trim_start_matches("e:")
                    .trim_start_matches("m:")
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_')
                    && j > 0 =>
            {
                rest = &tail[j + 1..];
            }
            _ => {
                out.push('{');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

fn strip_tags(s: &str) -> String {
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
    text
}

/// Текст без разметки, значков и пояса — по строкам, числа замаскированы.
pub fn lines(html: &str) -> Vec<String> {
    let mut s = html.replace("\\n", "\n");
    // Премиум-эмодзи: <tg-emoji …>…</tg-emoji> — значок, не текст.
    while let Some(i) = s.find("<tg-emoji") {
        let Some(j) = s[i..].find("</tg-emoji>") else {
            break;
        };
        s.replace_range(i..i + j + "</tg-emoji>".len(), "");
    }
    s = strip_tokens(&s);
    for tag in [
        "<br>",
        "<br/>",
        "</p>",
        "</h3>",
        "</h4>",
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
        "<pre>",
        "</pre>",
        "<tg-button-row>",
        "</tg-button-row>",
        "</div>",
        "<div",
        "<table",
        "</table>",
    ] {
        s = s.replace(tag, &format!("\n{tag}"));
    }
    s = s
        .replace("</td>", " </td>")
        .replace("</th>", " </th>")
        .replace("</span>", " </span>")
        .replace("<small", "  <small");
    let text = unescape(&strip_tags(&s)).replace(" МСК", "");
    text.lines()
        .map(|l| {
            let kept: String = l
                .chars()
                .filter(|c| !is_emoji(*c))
                .filter(|c| {
                    c.is_alphanumeric()
                        || c.is_whitespace()
                        || "·.,:;!?—–-()«»%$/+<>=\"'@_#…→⇄×&".contains(*c)
                })
                .collect();
            mask(&kept.split_whitespace().collect::<Vec<_>>().join(" "))
        })
        .filter(|l| !l.is_empty())
        .collect()
}

/// Подпись кнопки для сравнения: без значков, разметки, стрелки возврата; числа — «#».
pub fn label(s: &str) -> String {
    let s = unescape(&strip_tags(&strip_tokens(s)));
    let kept: String = s.chars().filter(|c| !is_emoji(*c)).collect();
    mask(kept.trim().trim_start_matches(['‹', '◀']).trim())
}

fn starts_with_emoji(s: &str) -> bool {
    s.trim().chars().next().is_some_and(is_emoji)
}

/// Кнопки из `reply_markup` (инлайн и обычная клавиатура) и из `<tg-button>` в карточке.
pub fn buttons(markup: &Value, rich: Option<&str>) -> Vec<Button> {
    let mut out = Vec::new();
    for key in ["inline_keyboard", "keyboard"] {
        for row in markup[key].as_array().into_iter().flatten() {
            for b in row.as_array().into_iter().flatten() {
                let text = b["text"].as_str().unwrap_or_default();
                out.push(Button {
                    label: label(text),
                    style: b["style"].as_str().map(str::to_owned),
                    icon: b["icon_custom_emoji_id"].is_string() || starts_with_emoji(text),
                });
            }
        }
    }
    if let Some(rich) = rich {
        for part in rich.split("<tg-button ").skip(1) {
            let Some(close) = part.find('>') else {
                continue;
            };
            let attrs = &part[..close];
            let inner = part[close + 1..]
                .split("</tg-button>")
                .next()
                .unwrap_or_default();
            let style = attrs
                .split("style=\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .filter(|s| *s != "link")
                .map(str::to_owned);
            out.push(Button {
                label: label(inner),
                style,
                icon: inner.contains("<tg-emoji") || starts_with_emoji(&strip_tags(inner)),
            });
        }
    }
    out
}

/// Расхождения экрана бота с записью реестра; пусто — по макету.
pub fn compare(entry: &Value, codes: &[&Code]) -> Vec<String> {
    let mut problems = Vec::new();
    let ours: String = codes
        .iter()
        .flat_map(|c| lines(c.rich.as_deref().unwrap_or(&c.html)))
        .collect::<Vec<_>>()
        .join(" \n");
    let messages = entry["messages"].as_array().cloned().unwrap_or_default();
    for m in &messages {
        for line in lines(m["html"].as_str().unwrap_or_default()) {
            if !ours.contains(&line) {
                problems.push(format!("текст: «{line}»"));
            }
        }
    }
    let Some(main) = messages.last() else {
        return problems;
    };
    if main["kind"] == "rich" && !codes.iter().any(|c| c.rich.is_some()) {
        problems.push("вид: в макете карточка, в боте обычное сообщение".into());
    }
    let all: Vec<&Button> = codes.iter().flat_map(|c| c.buttons.iter()).collect();
    for row in main["keys"].as_array().into_iter().flatten() {
        for k in row.as_array().into_iter().flatten() {
            let want = label(k["label"].as_str().unwrap_or_default());
            if want.is_empty() {
                continue;
            }
            let Some(b) = all.iter().find(|b| b.label == want) else {
                problems.push(format!("кнопка: «{want}» нет"));
                continue;
            };
            if let Some(style) = k["style"].as_str()
                && b.style.as_deref() != Some(style)
            {
                problems.push(format!(
                    "кнопка «{want}»: цвет {}, в макете {style}",
                    b.style.as_deref().unwrap_or("обычный")
                ));
            }
            if k["icon"].is_string() && !b.icon {
                problems.push(format!("кнопка «{want}»: без значка"));
            }
        }
    }
    problems
}

/// Сверить записи реестра одного бота: `match` — по макету, `todo` с экранами — ещё не по
/// макету (иначе книгу надо поправить). Возвращает ошибки и сводку.
pub fn check(bot: &str, screens: &BTreeMap<String, Code>) -> (Vec<String>, String) {
    let ledger = ledger();
    let mut errors = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for entry in registry().iter().filter(|e| e["bot"] == bot) {
        let id = entry["id"].as_str().unwrap_or_default();
        let Some(row) = ledger.get(id) else {
            errors.push(format!("{id}: нет в книге accepted-ledger.json"));
            continue;
        };
        let status = row["status"].as_str().unwrap_or_default();
        *counts.entry(status.to_owned()).or_default() += 1;
        if entry["later"] == true && status != "later" {
            errors.push(format!("{id}: виджет — статус «later»"));
        }
        let names: Vec<&str> = row["screens"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let codes: Vec<&Code> = names.iter().filter_map(|n| screens.get(*n)).collect();
        match status {
            "match" => {
                if names.is_empty() || codes.len() != names.len() {
                    errors.push(format!("{id}: нет экрана бота {names:?}"));
                    continue;
                }
                let problems = compare(entry, &codes);
                if !problems.is_empty() {
                    errors.push(format!(
                        "{id} ({}): {}",
                        names.join(", "),
                        problems.join("; ")
                    ));
                }
            }
            "todo" => {
                if !codes.is_empty()
                    && codes.len() == names.len()
                    && compare(entry, &codes).is_empty()
                {
                    errors.push(format!(
                        "{id}: уже по макету — переведите в «match» в accepted-ledger.json"
                    ));
                }
            }
            "superseded" => {
                let by = row["by"].as_str().unwrap_or_default();
                if !ledger.contains_key(by) {
                    errors.push(format!("{id}: заменён на {by} — такого экрана нет в книге"));
                }
            }
            "later" => {}
            other => errors.push(format!("{id}: неизвестный статус «{other}»")),
        }
    }
    let total: usize = counts.values().sum();
    let summary = format!(
        "{bot}: по макету {} из {} (исправить {}, заменены решениями {}, виджеты {})",
        counts.get("match").copied().unwrap_or(0),
        total
            - counts.get("later").copied().unwrap_or(0)
            - counts.get("superseded").copied().unwrap_or(0),
        counts.get("todo").copied().unwrap_or(0),
        counts.get("superseded").copied().unwrap_or(0),
        counts.get("later").copied().unwrap_or(0),
    );
    (errors, summary)
}

/// Книга и реестр — одно множество экранов.
pub fn ledger_covers_registry() -> Vec<String> {
    let ledger = ledger();
    let ids: Vec<String> = registry()
        .iter()
        .filter_map(|e| e["id"].as_str().map(str::to_owned))
        .collect();
    let mut errors: Vec<String> = ids
        .iter()
        .filter(|id| !ledger.contains_key(*id))
        .map(|id| format!("{id}: нет в книге"))
        .collect();
    errors.extend(
        ledger
            .keys()
            .filter(|k| !ids.contains(k))
            .map(|k| format!("{k}: в книге, но нет в реестре — пересоберите accepted.json")),
    );
    errors
}
