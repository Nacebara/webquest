//! Главное меню: текст кнопки → действие и обратно, терпимо к эмодзи, регистру, «ё» и
//! пробелам; при Premium — иконки и стиль `primary` у главных действий.
#![allow(clippy::unwrap_used)]

mod common;

use botapi::ButtonStyle;
use ui::MenuAction;

const LAYOUT: [&[MenuAction]; 3] = [
    &[MenuAction::Exchange, MenuAction::PayInvoice],
    &[MenuAction::Rates, MenuAction::History],
    &[MenuAction::Support],
];

#[test]
fn every_button_parses_back_in_both_modes() {
    for (mode, ui) in common::modes() {
        let menu = ui.main_menu();
        assert_eq!(menu.is_persistent, Some(true));
        assert_eq!(menu.resize_keyboard, Some(true));
        assert_eq!(menu.keyboard.len(), LAYOUT.len());
        for (row, expected) in menu.keyboard.iter().zip(LAYOUT) {
            assert_eq!(row.len(), expected.len(), "{mode}");
            for (button, action) in row.iter().zip(expected.iter()) {
                assert_eq!(
                    ui.parse_menu(&button.text),
                    Some(*action),
                    "{mode}: {}",
                    button.text
                );
            }
        }
    }
}

#[test]
fn texts_from_either_mode_and_typing_variants_are_accepted() {
    let plain = common::plain_ui();
    let premium = common::premium_ui();
    // Кнопка старой клавиатуры (другой режим) тоже должна работать.
    for button in premium.main_menu().keyboard.iter().flatten() {
        assert!(plain.parse_menu(&button.text).is_some(), "{}", button.text);
    }
    for button in plain.main_menu().keyboard.iter().flatten() {
        assert!(
            premium.parse_menu(&button.text).is_some(),
            "{}",
            button.text
        );
    }
    let cases = [
        ("💱 Обменять чек", MenuAction::Exchange),
        ("обменять чек", MenuAction::Exchange),
        ("  ОБМЕНЯТЬ   ЧЕК ", MenuAction::Exchange),
        ("🔁 Обменять чек!", MenuAction::Exchange),
        ("🧾 Оплатить счёт", MenuAction::PayInvoice),
        ("Оплатить счет", MenuAction::PayInvoice),
        ("📈Курс и лимиты", MenuAction::Rates),
        ("курс и ЛИМИТЫ", MenuAction::Rates),
        ("📜 История", MenuAction::History),
        ("история", MenuAction::History),
        ("💬 Поддержка", MenuAction::Support),
        ("⚠️ поддержка ⚠️", MenuAction::Support),
    ];
    for (text, action) in cases {
        assert_eq!(plain.parse_menu(text), Some(action), "{text}");
    }
    for text in [
        "",
        "💱",
        "Обменять",
        "чек",
        "/start",
        "https://t.me/send?start=CQabcdefghij",
        "обменять чек пожалуйста",
    ] {
        assert_eq!(plain.parse_menu(text), None, "{text}");
    }
}

#[test]
fn premium_menu_uses_icons_and_primary_style() {
    let premium = common::premium_ui().main_menu();
    for (row, expected) in premium.keyboard.iter().zip(LAYOUT) {
        for (button, action) in row.iter().zip(expected.iter()) {
            assert!(button.icon_custom_emoji_id.is_some(), "{}", button.text);
            assert!(
                button.text.chars().next().unwrap().is_alphabetic(),
                "при Premium эмодзи — иконкой: {}",
                button.text
            );
            let main = matches!(action, MenuAction::Exchange | MenuAction::PayInvoice);
            assert_eq!(
                button.style,
                main.then_some(ButtonStyle::Primary),
                "{}",
                button.text
            );
        }
    }
    let plain = common::plain_ui().main_menu();
    for button in plain.keyboard.iter().flatten() {
        assert!(button.icon_custom_emoji_id.is_none());
        assert!(button.style.is_none());
        assert!(
            !button.text.chars().next().unwrap().is_alphabetic(),
            "{}",
            button.text
        );
    }
}
