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
fn dash_four_states() {
    let t = Texts::builtin().unwrap();
    // Имена файлов — латиницей (Windows у владельца).
    for ((_, data), name) in ops::sample_dash()
        .iter()
        .zip(["ok", "fire", "calm", "pauses"])
    {
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
    // Д-19.10: админ видит лимит, но менять его — владельцу; лимит кончается; изменение.
    let admin = Look::plain().screen(&ops::sample_reserves(&t).screen_for(&t, false));
    insta::assert_snapshot!("reserves_admin", dump(&admin));
    let low = Look::plain().screen(&ops::sample_reserves_passcode_low(&t).screen(&t));
    insta::assert_snapshot!("reserves_passcode_low", dump(&low));
    let change = Look::plain().screen(&ops::sample_passcode_preview(&t));
    insta::assert_snapshot!("passcode_change", dump(&change));
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

/// Шаг 1.5 (утверждено владельцем 13.11): подсказка сверки в карточке заявки — «прошла»,
/// «не прошла», «не понять».
#[test]
fn order_card_reconcile_hint() {
    let t = Texts::builtin().unwrap();
    let links = ops::Links::default();
    for (done, name) in [
        (Some(true), "order_card_hint_done"),
        (Some(false), "order_card_hint_not"),
        (None, "order_card_hint_unclear"),
    ] {
        let mut v = ops::sample_order();
        v.reconcile = Some(ops::ReconcileHint {
            at: ops::sample_now(),
            platform: domain::Platform::XRocket,
            kind: "create_payout_check".into(),
            done,
        });
        let card = Look::plain().screen(&v.screen(
            &t,
            storage::staff::Role::Owner,
            ops::sample_now(),
            &links,
        ));
        insta::assert_snapshot!(name, dump(&card));
    }
}

/// Шаг 1.5: тревога о недостаче, когда сверка учла неясную выплату, и «сверка не проходит».
#[test]
fn reconcile_alerts_with_unsettled_operations() {
    let t = Texts::builtin().unwrap();
    // Макет: одна неясная выплата 97.50, денег меньше обоих исходов.
    let payload = serde_json::json!({
        "platform": "xrocket", "expected": "902.5", "ledger": "1000", "actual": "890.5",
        "diff": "-12", "paused": true, "account": "ub-1", "below_all": true,
        "unsettled": [{"id": 7, "order_id": 1, "public": "K7M2Q9XR4T", "kind": "create_payout_check", "status": "unknown", "amount": "97.5"}],
    });
    let short = Look::plain().screen(&ops::mismatch_screen(&t, &payload, 1));
    insta::assert_snapshot!("shortfall_unsettled", dump(&short));
    // Две неясные операции, баланс между исходами.
    let payload = serde_json::json!({
        "platform": "cryptobot", "expected": "1000", "ledger": "1000", "actual": "990",
        "diff": "-10", "paused": true, "account": "ub-1", "below_all": false,
        "unsettled": [
            {"id": 8, "order_id": 2, "public": "F6A7B8C9D0", "kind": "create_payout_check", "status": "unknown", "amount": "19.7"},
            {"id": 9, "order_id": 3, "public": "11AA22BB33", "kind": "create_refund_check", "status": "unknown", "amount": "50"},
        ],
    });
    let between = Look::plain().screen(&ops::mismatch_screen(&t, &payload, 2));
    insta::assert_snapshot!("shortfall_unsettled_between", dump(&between));

    let stalled = serde_json::json!({
        "platform": "xrocket", "since": "2026-11-05T10:40:00+00:00", "minutes": 35,
        "reason": "in_progress",
        "ops": [{"id": 7, "order_id": 1, "public": "K7M2Q9XR4T", "kind": "create_payout_check", "status": "dispatched", "amount": "97.5"}],
    });
    let screen = Look::plain().screen(&ops::stalled_screen(&t, &stalled, 3));
    insta::assert_snapshot!("reconcile_stalled", dump(&screen));
    let selftest = serde_json::json!({
        "platform": "cryptobot", "since": "2026-11-05T10:00:00+00:00", "minutes": 75,
        "reason": "selftest", "ops": [],
    });
    let screen = Look::plain().screen(&ops::stalled_screen(&t, &selftest, 4));
    insta::assert_snapshot!("reconcile_stalled_selftest", dump(&screen));
}

/// Критичные тревоги шага 2.8 (утверждены владельцем 14.11 и в макете пульта).
#[test]
fn critical_alerts() {
    let t = Texts::builtin().unwrap();
    let first = "2026-11-14T11:15:00Z".parse().unwrap();
    let cases = [
        (
            "login_lost",
            serde_json::json!({"account": "ub-2", "userbot_id": 2, "platforms": ["xrocket"]}),
        ),
        (
            "account_restricted",
            serde_json::json!({"account": "ub-1", "userbot_id": 1, "platforms": ["cryptobot"]}),
        ),
        (
            "flood_long",
            serde_json::json!({"account": "ub-1", "userbot_id": 1, "platforms": ["cryptobot"],
                "wait_secs": 2820, "until": "2026-11-14T12:02:00+00:00"}),
        ),
        (
            "engine_down",
            serde_json::json!({"since": "2026-11-14T11:09:00+00:00",
                "error": "another engine instance is running"}),
        ),
        (
            "second_instance",
            serde_json::json!({"error": "Conflict: terminated by other getUpdates request"}),
        ),
        (
            "unknown_reply",
            serde_json::json!({"platform": "xrocket", "text": "Чек CQ12…9F временно недоступен, попробуйте позже",
                "order": 41, "public": "7F3A1C9D2B", "direction": "xr_to_cb_check",
                "at": "2026-11-14T11:02:00+00:00"}),
        ),
        (
            "kill_switch",
            serde_json::json!({"by": "Олег", "at": "2026-11-14T11:20:00+00:00"}),
        ),
        // Макет «Новая авторизация»: «Новый вход · ub-1 / iPhone 15 · Telegram iOS · NL · 185.…41 · 13:58».
        (
            "new_login",
            serde_json::json!({"account": "ub-1", "hash": "7", "device": "iPhone 15",
                "app": "Telegram iOS", "country": "NL", "ip": "185.…41",
                "at": "2026-11-14T10:58:00+00:00"}),
        ),
    ];
    for (title, payload) in cases {
        let screen = ops::critical_screen(&t, title, &payload, 9, first).unwrap();
        let mut text = Look::plain().screen(&screen);
        // «· N мин» у «Движок не работает» считается от текущего времени — в снимке не нужен.
        if title == "engine_down" {
            text.text = text.text.split(" · ").next().unwrap().to_owned();
            text.rich = None;
        }
        insta::assert_snapshot!(format!("critical_{title}"), dump(&text));
    }
    assert!(ops::critical_screen(&t, "manual_review", &serde_json::json!({}), 1, first).is_none());
}

/// П-6, П-7: макет «Алерт и „Принял“» — публичный номер, путь, суммы, клиент, причина, ответ
/// кошелька цитатой; без ответа — «ответа нет» со временем разбора.
#[test]
fn review_alert() {
    let t = Texts::builtin().unwrap();
    let base = serde_json::json!({"id": 15, "public": "K7M2Q9XR4T", "user": 51234567,
        "direction": "cb_to_xr_check", "amount_in": "100", "amount_out": "97.5",
        "reason": "Выплата: нет ответа 30 с, чек не найден", "since": "2026-11-14T11:04:04Z"});
    let mut replied = base.clone();
    replied["reply"] = serde_json::json!({"who": "xRocket", "at": "2026-11-14T11:04:04Z",
        "text": "Чек CQ12…9F временно недоступен"});
    for (name, p) in [
        ("review_alert_no_reply", base),
        ("review_alert_reply", replied),
    ] {
        let screen = Look::plain().screen(&ops::review_alert_screen(&t, &p, 9));
        insta::assert_snapshot!(name, dump(&screen));
    }
}

/// П-13: макет «Обслуживание» — «до 15:30», «3 заявки довожу», «+15 мин» и синяя «Закончить»;
/// без срока — «с 15:00», без начатых заявок — так и сказано.
#[test]
fn maintenance_card() {
    let t = Texts::builtin().unwrap();
    let at = |s: &str| s.parse::<chrono::DateTime<chrono::Utc>>().unwrap();
    let since = Some(at("2026-11-14T12:00:00Z"));
    for (name, until, live) in [
        ("maintenance_until", Some(at("2026-11-14T12:30:00Z")), 3),
        ("maintenance_since_idle", None, 0),
    ] {
        let screen = Look::plain().screen(&ops::maintenance_screen(&t, until, since, live));
        insta::assert_snapshot!(name, dump(&screen));
    }
}

/// П-3 (аудит 16.11, картинка принята): у оператора при неясном исходе — «решает админ» и
/// «Позвать админа» вместо повтора и возврата, которые ему недоступны.
#[test]
fn order_card_operator_unknown_outcome() {
    let t = Texts::builtin().unwrap();
    let links = ops::Links::new(Some("exch_bot".into()));
    let mut v = ops::sample_order();
    for op in &mut v.card.operations {
        if op.key.ends_with("payout") {
            op.status = "unknown".into();
        }
    }
    let card = Look::plain().screen(&v.screen(
        &t,
        storage::staff::Role::Operator,
        ops::sample_now(),
        &links,
    ));
    let dump = dump(&card);
    assert!(dump.contains("решает админ"), "{dump}");
    assert!(
        !dump.contains("Повторить") && !dump.contains("Вернуть"),
        "{dump}"
    );
    insta::assert_snapshot!("order_card_operator_unknown", dump);
}

/// Д-19.11 (картинка принята 20.11): неясен возврат — «Вернуть» и «Возврат выдан», без
/// «Повторить выплату» и «Выплачено».
#[test]
fn order_card_unclear_refund() {
    let t = Texts::builtin().unwrap();
    let links = ops::Links::default();
    let mut v = ops::sample_order();
    v.card.refund_amount = Some(domain::Decimal::new(50, 2));
    for op in &mut v.card.operations {
        if op.key.ends_with("payout") {
            op.key = op.key.replace("payout", "refund");
            op.kind = "create_refund_check".into();
            op.status = "unknown".into();
        }
    }
    let card =
        Look::plain().screen(&v.screen(&t, storage::staff::Role::Owner, ops::sample_now(), &links));
    let dump = dump(&card);
    assert!(
        dump.contains("Возврат выдан") && dump.contains("Вернуть"),
        "{dump}"
    );
    assert!(
        !dump.contains("Повторить выплату") && !dump.contains("Выплачено"),
        "{dump}"
    );
    insta::assert_snapshot!("order_card_unclear_refund", dump);
}

/// Д-19.1 (картинка принята 20.11): заявка на оплату счёта выполнена, чек на переплату застрял —
/// строка-подсказка, «Переплата» в таблице, «Повторить чек переплаты» и «Чек выдан».
#[test]
fn order_card_stuck_overpayment() {
    let t = Texts::builtin().unwrap();
    let links = ops::Links::default();
    let mut v = ops::sample_order();
    v.card.state = domain::fsm::OrderState::Completed;
    v.card.direction = domain::Direction::PayCbInvoice;
    v.card.intake_platform = domain::Platform::XRocket;
    v.card.intake_amount = Some(domain::Decimal::new(6000, 2));
    v.card.invoice_amount = Some(domain::Decimal::new(5000, 2));
    v.card.payout_amount = None;
    v.card.fee_amount = Some(domain::Decimal::new(125, 2));
    v.card.refund_amount = Some(domain::Decimal::new(875, 2));
    v.card.manual_reason = None;
    for op in &mut v.card.operations {
        if op.key.ends_with("payout") {
            op.key = op.key.replace("payout", "excess-1");
            op.kind = "create_excess_check".into();
            op.status = "unknown".into();
            op.last_error = Some("чек xRocket — нет ответа, в истории не найден".into());
        }
    }
    for (role, name) in [
        (storage::staff::Role::Admin, "order_card_stuck_overpayment"),
        (
            storage::staff::Role::Operator,
            "order_card_stuck_overpayment_operator",
        ),
    ] {
        let card = Look::plain().screen(&v.screen(&t, role, ops::sample_now(), &links));
        let dump = dump(&card);
        assert!(dump.contains("Переплата не вернулась"), "{dump}");
        insta::assert_snapshot!(name, dump);
    }
}

/// Д-19.1: тревога «Нужен разбор» по застрявшей переплате — «Переплата 8.75: …», суммы счёта.
#[test]
fn review_alert_stuck_overpayment() {
    let t = Texts::builtin().unwrap();
    let p = serde_json::json!({"id": 17, "public": "Q1W2E3R4T5", "user": 51234567,
        "direction": "pay_cb_invoice", "amount_in": "60", "amount_out": "50",
        "excess": "8.75", "reason": "нет ответа 30 с, чек не найден",
        "since": "2026-11-14T11:13:58Z"});
    let screen = Look::plain().screen(&ops::review_alert_screen(&t, &p, 9));
    let dump = dump(&screen);
    assert!(
        dump.contains("Переплата 8.75: нет ответа 30 с, чек не найден"),
        "{dump}"
    );
    insta::assert_snapshot!("review_alert_stuck_overpayment", dump);
}
