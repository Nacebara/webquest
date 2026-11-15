//! Снимки всех экранов в обычном и Premium-виде (insta). В снимке — HTML, rich-HTML
//! (по строке на блок, для чтения) и клавиатура как JSON Bot API по ряду на строку.
//! Обновить: `INSTA_UPDATE=always cargo test -p ui --test snapshots`, затем прочитать diff.
#![allow(clippy::unwrap_used)]

mod common;

use serde_json::Value;
use ui::{Keyboard, Screen};

/// rich-HTML по строке на блок — только для читаемости снимка.
fn pretty_rich(rich: &str) -> String {
    let mut out = rich.to_owned();
    for tag in [
        "<h3>",
        "<h4>",
        "<p>",
        "<table",
        "<tr>",
        "<details",
        "<footer>",
        "<ul>",
        "<li>",
        "</details>",
        "</table>",
    ] {
        out = out.replace(tag, &format!("\n{tag}"));
    }
    out.trim_start().to_owned()
}

fn dump_keyboard(keyboard: &Keyboard) -> String {
    let Some(markup) = keyboard.reply_markup() else {
        return "--- keyboard: none".to_owned();
    };
    let mut value = serde_json::to_value(&markup).unwrap();
    let object = value.as_object_mut().unwrap();
    let (kind, rows) = if let Some(rows) = object.remove("inline_keyboard") {
        ("inline", rows)
    } else if let Some(rows) = object.remove("keyboard") {
        ("reply", rows)
    } else {
        ("remove", Value::Array(Vec::new()))
    };
    let mut out = format!("--- keyboard: {kind}");
    if !object.is_empty() {
        out.push(' ');
        out.push_str(&serde_json::to_string(&object).unwrap());
    }
    for row in rows.as_array().unwrap() {
        out.push('\n');
        out.push_str(&serde_json::to_string(row).unwrap());
    }
    out
}

fn dump(screen: &Screen) -> String {
    let mut out = format!("--- html\n{}\n", screen.html);
    if let Some(rich) = &screen.rich_html {
        out.push_str(&format!("--- rich\n{}\n", pretty_rich(rich)));
    }
    out.push_str(&dump_keyboard(&screen.keyboard));
    out
}

fn both(plain: &Screen, premium: &Screen) -> String {
    format!(
        "### plain\n{}\n\n### premium\n{}",
        dump(plain),
        dump(premium)
    )
}

#[test]
fn screens() {
    let plain = common::gallery(&common::plain_ui());
    let premium = common::gallery(&common::premium_ui());
    assert_eq!(plain.len(), premium.len());

    let mut errors = String::new();
    let mut refunds = String::new();
    for ((name, p), (name2, q)) in plain.iter().zip(&premium) {
        assert_eq!(name, name2);
        if let Some(code) = name.strip_prefix("error_") {
            errors.push_str(&format!("## {code}\n{}\n\n", both(p, q)));
        } else if let Some(reason) = name.strip_prefix("refund_started_") {
            refunds.push_str(&format!("## {reason}\n{}\n\n", both(p, q)));
        } else {
            insta::assert_snapshot!(name.as_str(), both(p, q));
        }
    }
    insta::assert_snapshot!("errors", errors.trim_end());
    insta::assert_snapshot!("refund_started", refunds.trim_end());
}

#[test]
fn main_menu_commands_and_rich_keyboard() {
    let mut out = String::new();
    for (mode, ui) in common::modes() {
        out.push_str(&format!(
            "### {mode}: main menu\n{}\n\n",
            dump_keyboard(&Keyboard::Reply(ui.main_menu()))
        ));
        let terms = ui.terms(&common::terms());
        out.push_str(&format!(
            "### {mode}: terms sent as rich\n{}\n\n",
            dump_keyboard(&ui.rich_keyboard(&terms))
        ));
    }
    let commands = common::plain_ui().bot_commands();
    out.push_str(&format!(
        "### setMyCommands\n{}",
        serde_json::to_string_pretty(&commands).unwrap()
    ));
    insta::assert_snapshot!("menu_and_commands", out);
}
