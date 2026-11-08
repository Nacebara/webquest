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

#[test]
fn reserves_and_shortfall() {
    let t = Texts::builtin().unwrap();
    let reserves = Look::plain().screen(&ops::sample_reserves(&t).screen(&t));
    insta::assert_snapshot!("reserves", dump(&reserves));
    let payload = serde_json::json!({
        "platform": "xrocket", "expected": "640", "actual": "628", "diff": "-12",
        "paused": true, "account": "ub-1",
    });
    let short = Look::plain().screen(&ops::mismatch_screen(&t, &payload, 1));
    insta::assert_snapshot!("shortfall", dump(&short));
    let payload = serde_json::json!({
        "platform": "cryptobot", "expected": "1000", "actual": "1250", "diff": "250",
        "paused": false, "account": "ub-1",
    });
    let extra = Look::plain().screen(&ops::mismatch_screen(&t, &payload, 2));
    insta::assert_snapshot!("surplus", dump(&extra));
}

#[test]
fn queue_and_order_card() {
    let t = Texts::builtin().unwrap();
    let links = ops::Links::new(Some("exch_bot".into()));
    let queue = Look::plain().screen(&ops::sample_queue().screen(&t, &links, 0));
    insta::assert_snapshot!("queue", dump(&queue));
    let empty = Look::plain().screen(&ops::QueueData::default().screen(&t, &links, 0));
    insta::assert_snapshot!("queue_empty", dump(&empty));
    for (role, name) in [
        (storage::staff::Role::Owner, "order_card_owner"),
        (storage::staff::Role::Operator, "order_card_operator"),
    ] {
        let card =
            Look::plain().screen(&ops::sample_order().screen(&t, role, ops::sample_now(), &links));
        insta::assert_snapshot!(name, dump(&card));
    }
}
