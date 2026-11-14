//! Шаг 6.2 плана (находка К-2; макет «Кнопка под каждой ошибкой» принят владельцем 14.11): под
//! каждой ошибкой — кнопка по смыслу. Без кнопки — только те, где остаётся подождать:
//! обслуживание (так в утверждённом макете), «слишком много заявок», «ещё проверяю прошлый чек».
#![allow(clippy::unwrap_used)]

mod common;

use ui::Keyboard;

const ONLY_WAIT: [&str; 4] = [
    "maintenance",
    "maintenance_until",
    "rate_limited",
    "invoice_check_busy",
];

#[test]
fn every_error_has_a_button_except_where_one_can_only_wait() {
    let ui = common::plain_ui();
    let mut problems = Vec::new();
    let errors = common::all_errors();
    assert!(errors.len() >= 40, "в списке все ошибки: {}", errors.len());
    for (name, error) in errors {
        let screen = ui.error(&error);
        let has = match &screen.keyboard {
            Keyboard::Inline(m) => m.inline_keyboard.iter().any(|row| !row.is_empty()),
            _ => false,
        };
        let wait = ONLY_WAIT.contains(&name);
        if wait && has {
            problems.push(format!("{name}: кнопка не нужна — только подождать"));
        }
        if !wait && !has {
            problems.push(format!("{name}: тупик — нет кнопки"));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}
