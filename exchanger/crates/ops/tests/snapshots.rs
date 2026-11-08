//! Снимки экранов пульта (CLAUDE.md п. 22): карточка (rich) и запасной HTML, кнопки.
#![allow(clippy::unwrap_used)]

use ops::{Look, Screen, Texts};

fn dump(s: &Screen) -> String {
    let rows: Vec<String> = s
        .keyboard
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect();
    format!(
        "--- rich\n{}\n--- html\n{}\n--- keyboard\n{}",
        s.rich.as_deref().unwrap_or("").replace("><", ">\n<"),
        s.text,
        rows.join("\n")
    )
}

#[test]
fn dash_three_states() {
    let t = Texts::builtin().unwrap();
    // Имена файлов — латиницей (Windows у владельца).
    for ((_, data), name) in ops::sample_dash().iter().zip(["ok", "fire", "calm"]) {
        let screen = Look::plain().screen(&data.screen(&t));
        insta::assert_snapshot!(format!("dash_{name}"), dump(&screen));
    }
}

#[test]
fn menu_owner_and_operator() {
    let t = Texts::builtin().unwrap();
    let busy = ops::MenuCounts {
        review: 2,
        inbox: 1,
        ..Default::default()
    };
    let owner = Look::plain().screen(&ops::menu_screen(&t, storage::staff::Role::Owner, &busy));
    insta::assert_snapshot!("menu_owner", dump(&owner));
    let quiet = ops::MenuCounts {
        kill: true,
        ..Default::default()
    };
    let operator = Look::plain().screen(&ops::menu_screen(
        &t,
        storage::staff::Role::Operator,
        &quiet,
    ));
    insta::assert_snapshot!("menu_operator_kill_switch", dump(&operator));
}
