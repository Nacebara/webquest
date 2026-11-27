//! Сторож макетов (решение владельца 21.11): каждый принятый экран клиента и инвестора учтён в
//! книге `docs/mockups/accepted-ledger.json`, и каждый «match» говорит то же, что макет — текст,
//! вид сообщения, кнопки, цвет, значки. Сводку «по макету N из M» печатает тест.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
#[path = "accepted/guard.rs"]
mod guard;

use std::collections::BTreeMap;

fn code(s: &ui::Screen) -> guard::Code {
    let markup = serde_json::to_value(s.keyboard.reply_markup()).unwrap();
    guard::Code {
        rich: s.rich_html.clone(),
        html: s.html.clone(),
        buttons: guard::buttons(&markup, s.rich_html.as_deref()),
    }
}

fn screens() -> BTreeMap<String, guard::Code> {
    let ui = common::premium_ui();
    let mut out = BTreeMap::new();
    for (name, s) in common::gallery(&ui) {
        out.insert(format!("gallery:{name}"), code(&s));
    }
    // Экран макета клиента — несколько сообщений бота: сверяются вместе.
    for p in ui::preview::client(&ui)
        .into_iter()
        .chain(ui::preview::client_supplements(&ui))
    {
        let parts: Vec<guard::Code> = p.messages.iter().map(code).collect();
        let rich = parts.iter().any(|c| c.rich.is_some());
        let text = parts
            .iter()
            .map(|c| c.rich.clone().unwrap_or_else(|| c.html.clone()))
            .collect::<Vec<_>>()
            .join("\n");
        out.insert(
            format!("preview:{}", p.title),
            guard::Code {
                rich: rich.then(|| text.clone()),
                html: text,
                buttons: parts.into_iter().flat_map(|c| c.buttons).collect(),
            },
        );
    }
    out
}

#[test]
fn every_accepted_screen_is_in_the_ledger() {
    let errors = guard::ledger_covers_registry();
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn accepted_client_screens_follow_the_mockups() {
    let (errors, summary) = guard::check("client", &screens());
    println!("{summary}");
    assert!(errors.is_empty(), "{summary}\n{errors:#?}");
}
