//! Сторож макетов пульта (решение владельца 21.11): каждый принятый экран пульта учтён в книге
//! `docs/mockups/accepted-ledger.json`, и каждый «match» говорит то же, что макет. Экраны бота —
//! снимки-тесты пульта (`tests/snapshots`, их сверяет insta с кодом).
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../ui/tests/accepted/guard.rs"]
mod guard;

use std::collections::BTreeMap;

fn section<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    text.split_once(start)
        .map(|(_, rest)| rest.split(end).next().unwrap_or(rest))
        .unwrap_or("")
}

fn screens() -> BTreeMap<String, guard::Code> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/snapshots");
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .replace("snapshots__", "");
        let text = std::fs::read_to_string(&path).unwrap();
        let rich = section(&text, "--- rich\n", "\n--- html\n").replace(">\n<", "><");
        let html = section(&text, "--- html\n", "\n--- keyboard").to_owned();
        let rows: Vec<serde_json::Value> = section(&text, "--- keyboard\n", "\u{0}")
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let markup = serde_json::json!({ "inline_keyboard": rows });
        let rich = (!rich.trim().is_empty()).then_some(rich);
        out.insert(
            format!("snap:{name}"),
            guard::Code {
                buttons: guard::buttons(&markup, rich.as_deref()),
                rich,
                html,
            },
        );
    }
    out
}

#[test]
fn accepted_panel_screens_follow_the_mockups() {
    let (errors, summary) = guard::check("ops", &screens());
    println!("{summary}");
    assert!(errors.is_empty(), "{summary}\n{errors:#?}");
}
