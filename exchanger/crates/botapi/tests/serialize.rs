//! Снимки JSON-тел запросов и разбор входящих объектов. Сравниваем с `serde_json::json!`:
//! тело запроса должно совпадать с ожидаемым целиком, без лишних `null`.

use botapi::{
    AnswerCallbackQuery, BotCommand, BotCommandScope, ButtonStyle, ChatType, DeleteMessage,
    EditMessageText, EntityKind, GetMe, GetUpdates, InlineButtonAction, InlineKeyboardButton,
    InlineKeyboardMarkup, KeyboardButton, Message, MessageOrigin, Method, ReplyKeyboardMarkup,
    ReplyKeyboardRemove, SendMessage, SendRichMessage, SetMyCommands, Update, UpdateKind,
    methods::parse_updates,
};
use serde_json::{Value, json};

fn to_json<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("сериализуется")
}

fn inline_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::url(
                "Забрать чек",
                "https://t.me/xrocket?start=t_abcdefghijklmno",
            )
            .style(ButtonStyle::Success),
            InlineKeyboardButton::copy_text(
                "Скопировать ссылку",
                "https://t.me/xrocket?start=t_abcdefghijklmno",
            )
            .icon("5368324170671202286"),
        ],
        vec![
            InlineKeyboardButton::callback("CryptoBot → xRocket", "dir:cb_to_xr_check")
                .style(ButtonStyle::Primary),
            InlineKeyboardButton::callback("Вернуть деньги", "refund:A1B2C3D4E5")
                .style(ButtonStyle::Danger),
            InlineKeyboardButton::disabled("xRocket → CryptoBot"),
        ],
    ])
}

#[test]
fn send_message_with_inline_keyboard() {
    let request = SendMessage::html(777, "<b>Готово!</b>").reply_markup(inline_keyboard());
    assert_eq!(SendMessage::NAME, "sendMessage");
    assert_eq!(
        to_json(&request),
        json!({
            "chat_id": 777,
            "text": "<b>Готово!</b>",
            "parse_mode": "HTML",
            "link_preview_options": {"is_disabled": true},
            "reply_markup": {"inline_keyboard": [
                [
                    {"text": "Забрать чек", "style": "success",
                     "url": "https://t.me/xrocket?start=t_abcdefghijklmno"},
                    {"text": "Скопировать ссылку", "icon_custom_emoji_id": "5368324170671202286",
                     "copy_text": {"text": "https://t.me/xrocket?start=t_abcdefghijklmno"}}
                ],
                [
                    {"text": "CryptoBot → xRocket", "style": "primary",
                     "callback_data": "dir:cb_to_xr_check"},
                    {"text": "Вернуть деньги", "style": "danger",
                     "callback_data": "refund:A1B2C3D4E5"},
                    {"text": "xRocket → CryptoBot", "disabled": true}
                ]
            ]}
        })
    );
}

#[test]
fn send_message_with_main_menu_reply_keyboard() {
    let menu = ReplyKeyboardMarkup {
        keyboard: vec![
            vec![
                KeyboardButton::new("Обменять чек")
                    .maybe_style(Some(ButtonStyle::Primary))
                    .maybe_icon(Some("5390000000000000001".into())),
                KeyboardButton::new("🧾 Оплатить счёт"),
            ],
            vec![KeyboardButton::new("💬 Поддержка")],
        ],
        is_persistent: Some(true),
        resize_keyboard: Some(true),
        one_time_keyboard: None,
        input_field_placeholder: Some("Пришлите чек или счёт".into()),
    };
    let request = SendMessage::html(1, "меню").silent().reply_markup(menu);
    assert_eq!(
        to_json(&request),
        json!({
            "chat_id": 1,
            "text": "меню",
            "parse_mode": "HTML",
            "link_preview_options": {"is_disabled": true},
            "disable_notification": true,
            "reply_markup": {
                "keyboard": [
                    [
                        {"text": "Обменять чек", "style": "primary",
                         "icon_custom_emoji_id": "5390000000000000001"},
                        {"text": "🧾 Оплатить счёт"}
                    ],
                    [{"text": "💬 Поддержка"}]
                ],
                "is_persistent": true,
                "resize_keyboard": true,
                "input_field_placeholder": "Пришлите чек или счёт"
            }
        })
    );
}

#[test]
fn remove_keyboard() {
    let request = SendMessage::html(1, "ok").reply_markup(ReplyKeyboardRemove::default());
    assert_eq!(
        to_json(&request)["reply_markup"],
        json!({"remove_keyboard": true})
    );
}

#[test]
fn send_rich_message() {
    let request = SendRichMessage::new(-100_123, "<h3>Курс и лимиты</h3>")
        .thread(5)
        .reply_markup(InlineKeyboardMarkup::new(vec![vec![
            InlineKeyboardButton::callback("Не отображается?", "plain"),
        ]]));
    assert_eq!(SendRichMessage::NAME, "sendRichMessage");
    assert_eq!(
        to_json(&request),
        json!({
            "chat_id": -100_123,
            "message_thread_id": 5,
            "rich_message": {"html": "<h3>Курс и лимиты</h3>", "skip_entity_detection": true},
            "reply_markup": {"inline_keyboard": [[{"text": "Не отображается?", "callback_data": "plain"}]]}
        })
    );
}

#[test]
fn edit_message_text_html_and_rich() {
    let html = EditMessageText::html(9, 100, "⏳ <b>Проверяю чек</b>").reply_markup(
        InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::copy_text(
            "📋 Номер заявки",
            "A1B2C3D4E5",
        )]]),
    );
    assert_eq!(EditMessageText::NAME, "editMessageText");
    assert_eq!(
        to_json(&html),
        json!({
            "chat_id": 9,
            "message_id": 100,
            "text": "⏳ <b>Проверяю чек</b>",
            "parse_mode": "HTML",
            "link_preview_options": {"is_disabled": true},
            "reply_markup": {"inline_keyboard": [[
                {"text": "📋 Номер заявки", "copy_text": {"text": "A1B2C3D4E5"}}
            ]]}
        })
    );

    let rich = EditMessageText::rich(9, 101, "<p>статистика</p>");
    assert_eq!(
        to_json(&rich),
        json!({
            "chat_id": 9,
            "message_id": 101,
            "rich_message": {"html": "<p>статистика</p>", "skip_entity_detection": true}
        })
    );
}

#[test]
fn small_methods() {
    assert_eq!(to_json(&GetMe {}), json!({}));
    assert_eq!(GetMe::NAME, "getMe");

    let poll = GetUpdates::after(Some(41), 30).allowed(vec![
        UpdateKind::Message,
        UpdateKind::EditedMessage,
        UpdateKind::CallbackQuery,
    ]);
    assert_eq!(
        to_json(&poll),
        json!({"offset": 42, "timeout": 30,
               "allowed_updates": ["message", "edited_message", "callback_query"]})
    );
    assert_eq!(to_json(&GetUpdates::after(None, 0)), json!({"timeout": 0}));

    assert_eq!(
        to_json(&AnswerCallbackQuery::ack("q1")),
        json!({"callback_query_id": "q1"})
    );
    assert_eq!(
        to_json(&AnswerCallbackQuery::notice(
            "q2",
            "Направление на паузе",
            true
        )),
        json!({"callback_query_id": "q2", "text": "Направление на паузе", "show_alert": true})
    );
    assert_eq!(
        to_json(&AnswerCallbackQuery::notice("q3", "Скопировано", false)),
        json!({"callback_query_id": "q3", "text": "Скопировано"})
    );
    assert_eq!(
        to_json(&DeleteMessage {
            chat_id: 5,
            message_id: 6
        }),
        json!({"chat_id": 5, "message_id": 6})
    );

    let mut commands = SetMyCommands::new(vec![
        BotCommand::new("start", "Начать"),
        BotCommand::new("terms", "Курс и лимиты"),
    ]);
    commands.scope = Some(BotCommandScope::AllPrivateChats);
    commands.language_code = Some("ru".into());
    assert_eq!(
        to_json(&commands),
        json!({
            "commands": [
                {"command": "start", "description": "Начать"},
                {"command": "terms", "description": "Курс и лимиты"}
            ],
            "scope": {"type": "all_private_chats"},
            "language_code": "ru"
        })
    );
    assert_eq!(
        to_json(&BotCommandScope::Chat { chat_id: 7 }),
        json!({"type": "chat", "chat_id": 7})
    );
}

#[test]
fn inline_button_round_trip_and_tolerant_parsing() {
    let keyboard = inline_keyboard();
    let back: InlineKeyboardMarkup = serde_json::from_value(to_json(&keyboard)).unwrap();
    assert_eq!(back, keyboard);

    // Кнопки чужих ботов: незнакомые действия и стиль не ломают разбор.
    let foreign: InlineKeyboardMarkup = serde_json::from_value(json!({"inline_keyboard": [[
        {"text": "Open app", "web_app": {"url": "https://app.send.tg"}},
        {"text": "Pay", "pay": true, "style": "rainbow"},
        {"text": "Check", "url": "https://t.me/send?start=CQabcdefghij", "extra": 1}
    ]]}))
    .unwrap();
    let buttons: Vec<_> = foreign.buttons().collect();
    assert_eq!(buttons[0].action, InlineButtonAction::Other);
    assert_eq!(buttons[1].style, None);
    assert_eq!(
        buttons[2].action,
        InlineButtonAction::Url("https://t.me/send?start=CQabcdefghij".into())
    );
}

fn forwarded_check_update() -> Value {
    json!({
        "update_id": 1000,
        "message": {
            "message_id": 55,
            "from": {"id": 10, "is_bot": false, "first_name": "Ann", "username": "ann", "is_premium": true},
            "chat": {"id": 10, "type": "private", "first_name": "Ann"},
            "date": 1_790_000_000,
            "forward_origin": {"type": "user", "date": 1_789_999_000,
                               "sender_user": {"id": 1_559_501_630, "is_bot": true, "first_name": "Crypto Bot", "username": "send"}},
            "text": "🦋 Чек на 10 USDT\nhttps://t.me/send?start=CQaaaaaaaaaa и ещё ссылка",
            "entities": [
                {"type": "bold", "offset": 0, "length": 16},
                {"type": "url", "offset": 18, "length": 36},
                {"type": "text_link", "offset": 61, "length": 6, "url": "https://t.me/send?start=CQbbbbbbbbbb"},
                {"type": "date_time", "offset": 0, "length": 1, "unix_time": 1}
            ],
            "reply_markup": {"inline_keyboard": [[
                {"text": "Получить 10 USDT", "url": "https://t.me/send?start=CQcccccccccc"}
            ]]},
            "some_future_field": {"nested": true}
        }
    })
}

#[test]
fn message_update_with_entities_forward_and_buttons() {
    let update: Update = serde_json::from_value(forwarded_check_update()).unwrap();
    let message: &Message = update.message.as_ref().unwrap();
    assert_eq!(update.chat_id(), Some(10));
    assert_eq!(update.from().map(|u| u.id), Some(10));
    assert_eq!(message.chat.kind, ChatType::Private);
    assert_eq!(message.entities[3].kind, EntityKind::Unknown);
    let Some(MessageOrigin::User { sender_user, .. }) = &message.forward_origin else {
        panic!("ожидалась пересылка от пользователя");
    };
    assert_eq!(sender_user.username.as_deref(), Some("send"));
    assert_eq!(
        message.entity_urls(),
        vec![
            "https://t.me/send?start=CQaaaaaaaaaa".to_owned(),
            "https://t.me/send?start=CQbbbbbbbbbb".to_owned()
        ]
    );
    assert_eq!(
        message.button_urls(),
        vec!["https://t.me/send?start=CQcccccccccc".to_owned()]
    );
}

#[test]
fn inline_check_sent_via_bot_and_caption_entities() {
    let message: Message = serde_json::from_value(json!({
        "message_id": 7,
        "chat": {"id": 10, "type": "private"},
        "date": 1,
        "via_bot": {"id": 5_014_831_088_i64, "is_bot": true, "first_name": "xRocket", "username": "xrocket"},
        "caption": "Чек t.me/xrocket?start=t_abcdefghijklmno",
        "caption_entities": [{"type": "url", "offset": 4, "length": 36}]
    }))
    .unwrap();
    assert_eq!(
        message.via_bot.as_ref().and_then(|b| b.username.as_deref()),
        Some("xrocket")
    );
    assert_eq!(
        message.entity_urls(),
        vec!["t.me/xrocket?start=t_abcdefghijklmno".to_owned()]
    );
}

#[test]
fn callback_query_with_inaccessible_message() {
    let update: Update = serde_json::from_value(json!({
        "update_id": 2,
        "callback_query": {
            "id": "4382bfdwdsb323b2d9",
            "from": {"id": 10, "is_bot": false, "first_name": "Ann"},
            "message": {"chat": {"id": 10, "type": "private"}, "message_id": 3, "date": 0},
            "chat_instance": "-1",
            "data": "order:A1B2C3D4E5"
        }
    }))
    .unwrap();
    let query = update.callback_query.as_ref().unwrap();
    assert!(query.message.as_ref().unwrap().is_inaccessible());
    assert_eq!(query.data.as_deref(), Some("order:A1B2C3D4E5"));
    assert_eq!(update.chat_id(), Some(10));
}

#[test]
fn unknown_and_broken_updates_still_advance_offset() {
    let raw = vec![
        forwarded_check_update(),
        // Незнакомый вид обновления — пустое обновление с id.
        json!({"update_id": 1001, "message_reaction": {"chat": {"id": 1}}}),
        // Битое известное поле — тоже пустое обновление с id.
        json!({"update_id": 1002, "message": {"message_id": "not a number"}}),
        // Без update_id — пропускается.
        json!({"oops": true}),
    ];
    let updates = parse_updates(raw);
    let ids: Vec<i64> = updates.iter().map(|u| u.update_id).collect();
    assert_eq!(ids, vec![1000, 1001, 1002]);
    assert!(updates[1].message.is_none() && updates[2].message.is_none());
}
