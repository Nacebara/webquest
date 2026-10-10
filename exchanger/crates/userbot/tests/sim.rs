//! Симулятор ботов кошельков (DESIGN-v0.2 §5): каждый ответ, меню, инлайн, счета, сбои.
#![cfg(feature = "sim")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use domain::{Asset, Decimal, Money, Platform};
use rust_decimal_macros::dec;
use tokio::sync::broadcast;
use userbot::sim::{
    AccountId, Call, CheckOrigin, CheckSpec, CheckStatus, ClaimOutcome, Fault, InvoiceSpec,
    SimInvoiceStatus, SimTransport, SimWorld, WebAppPayment, texts,
};
use userbot::transport::{ButtonKind, Chat, RawMessage, Transport, TransportError};

const CB: Platform = Platform::CryptoBot;
const XR: Platform = Platform::XRocket;
const CB_CHAT: Chat = Chat::WalletBot(Platform::CryptoBot);
const XR_CHAT: Chat = Chat::WalletBot(Platform::XRocket);

fn usdt(x: Decimal) -> Money {
    Money::new(x, Asset::Usdt).unwrap()
}

fn ton(x: Decimal) -> Money {
    Money::new(x, Asset::Ton).unwrap()
}

struct Env {
    world: SimWorld,
    ub: AccountId,
    t: SimTransport,
    client: AccountId,
}

fn env() -> Env {
    let world = SimWorld::new();
    let ub = world.add_account("exch_cb");
    let client = world.add_account("client");
    let t = world.transport(ub);
    Env {
        world,
        ub,
        t,
        client,
    }
}

fn drain(rx: &mut broadcast::Receiver<RawMessage>) -> Vec<RawMessage> {
    let mut out = Vec::new();
    while let Ok(m) = rx.try_recv() {
        out.push(m);
    }
    out
}

/// Ровно одно новое сообщение бота в потоке.
fn one(rx: &mut broadcast::Receiver<RawMessage>) -> RawMessage {
    let mut msgs = drain(rx);
    assert_eq!(msgs.len(), 1, "ожидали одно сообщение: {msgs:#?}");
    msgs.remove(0)
}

fn data(msg: &RawMessage, button: &str) -> Vec<u8> {
    match &msg
        .find_button(button)
        .unwrap_or_else(|| panic!("нет кнопки {button}: {msg:#?}"))
        .kind
    {
        ButtonKind::Callback(d) => d.clone(),
        other => panic!("кнопка {button} не callback: {other:?}"),
    }
}

fn start_param(url: &str) -> &str {
    url.split("start=").nth(1).unwrap()
}

async fn start(t: &SimTransport, chat: Chat, code: &str, rid: i64) -> RawMessage {
    t.send_text(chat, &format!("/start {code}"), rid)
        .await
        .unwrap()
}

// ---------- активация ----------

#[tokio::test]
async fn activation_moves_money_and_notifies_creator() {
    let Env {
        world,
        ub,
        t,
        client,
    } = env();
    world
        .set_balance(client, CB, Asset::Usdt, dec!(150))
        .unwrap();
    let check = world
        .create_check(CheckSpec::new(CB, usdt(dec!(100))).by(client))
        .unwrap();
    assert_eq!(world.balance(client, CB, Asset::Usdt), dec!(50));
    assert!(
        check.code.starts_with("CQ") && check.code.len() == 12,
        "{}",
        check.code
    );
    assert_eq!(check.url, format!("https://t.me/send?start={}", check.code));
    let total = world.total_in_world(CB, Asset::Usdt);

    let mut rx = t.subscribe();
    let sent = start(&t, CB_CHAT, &check.code, 1).await;
    assert!(sent.out);
    assert_eq!(sent.text, format!("/start {}", check.code));

    let reply = one(&mut rx);
    assert_eq!(reply.text, "Вы получили 100 USDT.");
    assert!(!reply.out);
    assert!(reply.id > sent.id);
    assert_eq!(reply.chat, CB_CHAT);

    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(100));
    let check = world.check(&check.code).unwrap();
    assert_eq!(check.status, CheckStatus::Claimed);
    assert_eq!(check.claimed_by, vec![ub]);
    assert_eq!(world.total_in_world(CB, Asset::Usdt), total);

    let notice = world.messages(client, CB_CHAT).pop().unwrap();
    assert_eq!(
        notice.text,
        format!(
            "Ваш чек активировал @exch_cb.\n\nЧек на 100 USDT — {}",
            check.url
        )
    );
    assert_eq!(notice.entity_urls, vec![check.url.clone()]);
}

#[tokio::test]
async fn second_start_with_new_random_id_says_already_activated() {
    let Env { world, ub, t, .. } = env();
    let url = world.client_check(CB, usdt(dec!(5))).unwrap();
    let code = start_param(&url).to_owned();
    let mut rx = t.subscribe();
    start(&t, CB_CHAT, &code, 1).await;
    assert_eq!(one(&mut rx).text, "Вы получили 5 USDT.");
    // Ловушка lovec (AUDIT.md:46): повтор с НОВЫМ random_id после выигрыша.
    start(&t, CB_CHAT, &code, 2).await;
    assert_eq!(one(&mut rx).text, texts::ALREADY_ACTIVATED);
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(5));
}

#[tokio::test]
async fn every_rejection_class_has_its_text_and_moves_no_money() {
    let Env {
        world,
        ub,
        t,
        client,
    } = env();
    world.set_balance(ub, CB, Asset::Usdt, dec!(10)).unwrap();
    let other = world.add_account("someone");
    let m = usdt(dec!(3));
    let mk = |spec: CheckSpec| world.create_check(spec).unwrap().code;
    let cases: Vec<(Chat, String, &str)> = vec![
        (CB_CHAT, "CQAAAAAAAAAA".into(), "Чек не найден."),
        (
            XR_CHAT,
            "mci_AAAAAAAAAAAAAAA".into(),
            "Мульти-чек не найден.",
        ),
        (
            XR_CHAT,
            "mc_AAAAAAAAAAAAAAA".into(),
            "Мульти-чек не найден.",
        ),
        (XR_CHAT, mk(CheckSpec::new(CB, m)), "Чек не найден."),
        (
            CB_CHAT,
            mk(CheckSpec::new(CB, m).personal_for(other)),
            "Вы не можете активировать этот чек.",
        ),
        (
            CB_CHAT,
            mk(CheckSpec::new(CB, m).with_subscription("@wallet_news")),
            "Подпишитесь на канал @wallet_news, чтобы активировать этот чек.",
        ),
        (
            CB_CHAT,
            mk(CheckSpec::new(CB, m).with_captcha()),
            "Введите символы, которые вы видите на картинке.",
        ),
        (
            CB_CHAT,
            mk(CheckSpec::new(CB, m).with_password("pw")),
            "Введите пароль от чека.",
        ),
        (
            CB_CHAT,
            mk(CheckSpec::new(CB, m).premium_only()),
            texts::PREMIUM_ONLY,
        ),
        (
            XR_CHAT,
            mk(CheckSpec::new(XR, m).premium_only()),
            texts::PREMIUM_ONLY,
        ),
    ];
    let mut rx = t.subscribe();
    for (i, (chat, code, expected)) in cases.iter().enumerate() {
        start(&t, *chat, code, 100 + i64::try_from(i).unwrap()).await;
        assert_eq!(one(&mut rx).text, *expected, "код {code}");
    }
    // Свой чек: бот показывает его, но не зачисляет.
    let own = world.create_check(CheckSpec::new(CB, m).by(ub)).unwrap();
    start(&t, CB_CHAT, &own.code, 200).await;
    assert!(
        one(&mut rx)
            .text
            .contains("Это ваш собственный чек на 3 USDT")
    );
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(7));
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(0));
    assert_eq!(
        world.claim_check(client, &own.url).unwrap(),
        ClaimOutcome::Received(m)
    );
}

#[tokio::test]
async fn subscription_and_premium_pass_when_satisfied() {
    let Env { world, ub, t, .. } = env();
    let m = usdt(dec!(2));
    let sub = world
        .create_check(CheckSpec::new(CB, m).with_subscription("@chan"))
        .unwrap();
    let prem = world
        .create_check(CheckSpec::new(XR, m).premium_only())
        .unwrap();
    let personal = world
        .create_check(CheckSpec::new(CB, m).personal_for(ub))
        .unwrap();
    world.join_channel(ub, "@chan").unwrap();
    world.set_premium(ub, true).unwrap();
    let mut rx = t.subscribe();
    start(&t, CB_CHAT, &sub.code, 1).await;
    assert_eq!(one(&mut rx).text, "Вы получили 2 USDT.");
    start(&t, XR_CHAT, &prem.code, 2).await;
    assert_eq!(one(&mut rx).text, "🚀 Вы получили 2 USDT");
    start(&t, CB_CHAT, &personal.code, 3).await;
    assert_eq!(one(&mut rx).text, "Вы получили 2 USDT.");
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(4));
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(2));
}

#[tokio::test]
async fn password_check_accepts_only_the_right_password() {
    let Env { world, ub, t, .. } = env();
    let check = world
        .create_check(CheckSpec::new(CB, usdt(dec!(1))).with_password("s3cret"))
        .unwrap();
    let mut rx = t.subscribe();
    start(&t, CB_CHAT, &check.code, 1).await;
    assert_eq!(one(&mut rx).text, texts::PASSWORD_PROMPT);
    t.send_text(CB_CHAT, "wrong", 2).await.unwrap();
    assert_eq!(one(&mut rx).text, texts::WRONG_PASSWORD);
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(0));
    t.send_text(CB_CHAT, "s3cret", 3).await.unwrap();
    assert_eq!(one(&mut rx).text, "Вы получили 1 USDT.");
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(1));
}

#[tokio::test]
async fn xrocket_fresh_check_is_not_found_once_then_succeeds() {
    let Env { world, ub, t, .. } = env();
    world.set_xrocket_fresh_not_found(Some(Duration::from_millis(3000)));
    let check = world
        .create_check(CheckSpec::new(XR, usdt(dec!(10))))
        .unwrap();
    assert!(
        check.code.starts_with("t_") && check.code.len() == 17,
        "{}",
        check.code
    );
    assert_eq!(
        check.url,
        format!("https://t.me/xrocket?start={}", check.code)
    );
    let mut rx = t.subscribe();
    start(&t, XR_CHAT, &check.code, 1).await;
    assert_eq!(one(&mut rx).text, "Чек не найден.");
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(0));
    start(&t, XR_CHAT, &check.code, 2).await;
    assert_eq!(one(&mut rx).text, "🚀 Вы получили 10 USDT");
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(10));

    // Чек старше окна — сразу успех; CryptoBot так не делает.
    let old = world
        .create_check(CheckSpec::new(XR, usdt(dec!(1))))
        .unwrap();
    world.advance(Duration::from_secs(5));
    start(&t, XR_CHAT, &old.code, 3).await;
    assert_eq!(one(&mut rx).text, "🚀 Вы получили 1 USDT");
    let cb = world
        .create_check(CheckSpec::new(CB, usdt(dec!(1))))
        .unwrap();
    start(&t, CB_CHAT, &cb.code, 4).await;
    assert_eq!(one(&mut rx).text, "Вы получили 1 USDT.");
}

#[tokio::test]
async fn multi_check_gives_each_user_one_activation() {
    let Env {
        world,
        ub,
        t,
        client,
    } = env();
    let check = world
        .create_check(CheckSpec::new(XR, ton(dec!(0.5))).multi(2))
        .unwrap();
    assert!(check.code.starts_with("mci_") && check.code.len() == 19);
    let mut rx = t.subscribe();
    start(&t, XR_CHAT, &check.code, 1).await;
    assert_eq!(one(&mut rx).text, "🚀 Вы получили 0.5 TONCOIN");
    start(&t, XR_CHAT, &check.code, 2).await;
    assert_eq!(one(&mut rx).text, texts::ALREADY_ACTIVATED);
    assert_eq!(
        world.claim_check(client, &check.url).unwrap(),
        ClaimOutcome::Received(ton(dec!(0.5)))
    );
    let third = world.add_account("late");
    assert_eq!(
        world.claim_check(third, &check.code).unwrap(),
        ClaimOutcome::AlreadyActivated
    );
    assert_eq!(
        world.check(&check.code).unwrap().status,
        CheckStatus::Claimed
    );
    assert_eq!(world.balance(ub, XR, Asset::Ton), dec!(0.5));
}

#[tokio::test]
async fn random_id_duplicate_on_send_text_has_no_side_effects() {
    let Env { world, ub, t, .. } = env();
    let url = world.client_check(CB, usdt(dec!(7))).unwrap();
    let mut rx = t.subscribe();
    start(&t, CB_CHAT, start_param(&url), 42).await;
    assert_eq!(one(&mut rx).text, "Вы получили 7 USDT.");
    let before = world.messages(ub, CB_CHAT);
    let err = t
        .send_text(CB_CHAT, &format!("/start {}", start_param(&url)), 42)
        .await
        .unwrap_err();
    assert_eq!(err, TransportError::RandomIdDuplicate);
    assert!(drain(&mut rx).is_empty());
    assert_eq!(world.messages(ub, CB_CHAT), before);
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(7));
    // random_id общий для всех чатов аккаунта и для send_inline.
    assert_eq!(
        t.send_text(Chat::SavedMessages, "note", 42)
            .await
            .unwrap_err(),
        TransportError::RandomIdDuplicate
    );
}

// ---------- меню чеков ----------

#[tokio::test]
async fn cryptobot_menu_creates_exactly_one_check_per_random_id() {
    let Env {
        world,
        ub,
        t,
        client,
    } = env();
    world.set_balance(ub, CB, Asset::Usdt, dec!(50)).unwrap();
    let mut rx = t.subscribe();

    t.send_text(CB_CHAT, "/checks", 1).await.unwrap();
    let menu = one(&mut rx);
    assert!(menu.text.contains("Чеки"));
    t.press(CB_CHAT, menu.id, &data(&menu, "Создать чек"))
        .await
        .unwrap();
    let assets = one(&mut rx);
    assert_eq!(assets.id, menu.id, "меню правится на месте");
    assert!(assets.edit_date.is_some());
    assert!(assets.find_button("USDT").is_some() && assets.find_button("TON").is_some());
    t.press(CB_CHAT, menu.id, &data(&assets, "USDT"))
        .await
        .unwrap();
    let ask = one(&mut rx);
    assert_eq!(
        ask.text,
        "Отправьте сумму чека в USDT.\n\nДоступно: 50 USDT."
    );

    t.send_text(CB_CHAT, "10", 100).await.unwrap();
    let created = one(&mut rx);
    let check = world.checks_created_by(ub).pop().unwrap();
    let link = format!("https://t.me/send?start={}", check.code);
    assert!(created.text.contains(&link), "{}", created.text);
    assert!(created.text.contains("Чек на 10 USDT"));
    assert_eq!(created.entity_urls, vec![link.clone()]);
    assert_eq!(created.button_urls(), vec![link.clone()]);
    assert_eq!(check.origin, CheckOrigin::Menu { random_id: 100 });
    assert_eq!(check.amount, usdt(dec!(10)));
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(40));

    // Повтор отправки суммы с тем же random_id — второго чека нет.
    assert_eq!(
        t.send_text(CB_CHAT, "10", 100).await.unwrap_err(),
        TransportError::RandomIdDuplicate
    );
    // С новым random_id диалог уже закрыт — тоже без чека.
    t.send_text(CB_CHAT, "10", 101).await.unwrap();
    assert_eq!(one(&mut rx).text, texts::UNKNOWN_COMMAND);
    assert_eq!(world.checks_created_by(ub).len(), 1);
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(40));

    // «Мои чеки» показывает ссылку.
    t.send_text(CB_CHAT, "/checks", 102).await.unwrap();
    let menu = one(&mut rx);
    t.press(CB_CHAT, menu.id, &data(&menu, "Мои чеки"))
        .await
        .unwrap();
    let list = one(&mut rx);
    assert!(list.text.contains(&link));
    assert_eq!(list.button_urls(), vec![link.clone()]);

    // Клиент забирает чек — создателю уведомление, список пуст.
    assert_eq!(
        world.claim_check(client, &link).unwrap(),
        ClaimOutcome::Received(usdt(dec!(10)))
    );
    assert_eq!(
        one(&mut rx).text,
        format!("Ваш чек активировал @client.\n\nЧек на 10 USDT — {link}")
    );
    t.send_text(CB_CHAT, "/checks", 103).await.unwrap();
    let menu = one(&mut rx);
    t.press(CB_CHAT, menu.id, &data(&menu, "Мои чеки"))
        .await
        .unwrap();
    assert_eq!(one(&mut rx).text, texts::NO_ACTIVE_CHECKS);
}

#[tokio::test]
async fn xrocket_menu_creates_personal_check() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, XR, Asset::Usdt, dec!(30)).unwrap();
    let mut rx = t.subscribe();
    t.send_text(XR_CHAT, "/cheques", 1).await.unwrap();
    let menu = one(&mut rx);
    t.press(XR_CHAT, menu.id, &data(&menu, "Персональный"))
        .await
        .unwrap();
    let personal = one(&mut rx);
    t.press(XR_CHAT, menu.id, &data(&personal, "Создать чек"))
        .await
        .unwrap();
    let assets = one(&mut rx);
    assert!(assets.find_button("TONCOIN").is_some());
    t.press(XR_CHAT, menu.id, &data(&assets, "USDT"))
        .await
        .unwrap();
    assert_eq!(
        one(&mut rx).text,
        "Отправьте сумму чека в USDT.\n\nБаланс: 30 USDT."
    );
    t.send_text(XR_CHAT, "12,5", 2).await.unwrap();
    let created = one(&mut rx);
    let check = world.checks_created_by(ub).pop().unwrap();
    assert!(check.code.starts_with("t_") && check.code.len() == 17);
    let link = format!("https://t.me/xrocket?start={}", check.code);
    assert!(created.text.contains(&link));
    assert_eq!(created.button_urls(), vec![link]);
    assert_eq!(check.amount, usdt(dec!(12.5)));
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(17.5));

    // Мульти-чек — всплывающее уведомление, без правки.
    t.send_text(XR_CHAT, "/cheques", 3).await.unwrap();
    let menu = one(&mut rx);
    let answer = t
        .press(XR_CHAT, menu.id, &data(&menu, "Мульти-чек"))
        .await
        .unwrap();
    assert!(answer.alert);
    assert!(drain(&mut rx).is_empty());
}

#[tokio::test]
async fn menu_reports_insufficient_funds_and_bad_amount() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, CB, Asset::Usdt, dec!(5)).unwrap();
    world.set_balance(ub, XR, Asset::Usdt, dec!(5)).unwrap();
    let mut rx = t.subscribe();

    t.send_text(CB_CHAT, "/checks", 1).await.unwrap();
    let menu = one(&mut rx);
    t.press(CB_CHAT, menu.id, &data(&menu, "Создать чек"))
        .await
        .unwrap();
    let assets = one(&mut rx);
    t.press(CB_CHAT, menu.id, &data(&assets, "USDT"))
        .await
        .unwrap();
    one(&mut rx);
    t.send_text(CB_CHAT, "abc", 2).await.unwrap();
    assert_eq!(one(&mut rx).text, texts::BAD_AMOUNT);
    t.send_text(CB_CHAT, "10", 3).await.unwrap();
    assert_eq!(one(&mut rx).text, "Недостаточно средств. Доступно: 5 USDT.");

    t.send_text(XR_CHAT, "/cheques", 4).await.unwrap();
    let menu = one(&mut rx);
    t.press(XR_CHAT, menu.id, &data(&menu, "Персональный"))
        .await
        .unwrap();
    let p = one(&mut rx);
    t.press(XR_CHAT, menu.id, &data(&p, "Создать чек"))
        .await
        .unwrap();
    let a = one(&mut rx);
    t.press(XR_CHAT, menu.id, &data(&a, "USDT")).await.unwrap();
    one(&mut rx);
    t.send_text(XR_CHAT, "5.01", 5).await.unwrap();
    assert_eq!(one(&mut rx).text, "Недостаточно средств. Доступно: 5 USDT.");

    assert!(world.checks().is_empty());
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(5));
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(5));
}

// ---------- инлайн ----------

#[tokio::test]
async fn inline_check_is_created_at_send_time_once_per_random_id() {
    let Env {
        world,
        ub,
        t,
        client,
    } = env();
    world.set_balance(ub, CB, Asset::Usdt, dec!(50)).unwrap();
    let results = t.inline_query(CB, "10usdt").await.unwrap();
    let ids: Vec<&str> = results.results.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["check-USDT", "invoice-USDT"]);
    assert_eq!(
        results.results[0].title.as_deref(),
        Some("Отправить 10 USDT")
    );
    assert!(world.checks().is_empty(), "запрос не создаёт чек");
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(50));

    let sent = t
        .send_inline(Chat::SavedMessages, CB, &results, "check-USDT", 555)
        .await
        .unwrap();
    assert!(sent.out);
    assert_eq!(sent.chat, Chat::SavedMessages);
    assert_eq!(sent.via_bot, Some(CB));
    assert_eq!(sent.text, "🦋 Чек на 10 USDT");
    let urls = sent.button_urls();
    assert_eq!(urls.len(), 1);
    assert!(urls[0].starts_with("https://t.me/send?start=CQ"));
    assert!(sent.entity_urls.is_empty(), "ссылка — только в кнопке");
    let check = world.checks_created_by(ub).pop().unwrap();
    assert_eq!(check.url, urls[0]);
    assert_eq!(check.origin, CheckOrigin::Inline { random_id: 555 });
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(40));

    assert_eq!(
        t.send_inline(Chat::SavedMessages, CB, &results, "check-USDT", 555)
            .await
            .unwrap_err(),
        TransportError::RandomIdDuplicate
    );
    assert_eq!(world.checks().len(), 1);
    assert_eq!(world.messages(ub, Chat::SavedMessages).len(), 1);
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(40));

    // Ссылку из кнопки клиент активирует; юзербот получает уведомление.
    let mut rx = t.subscribe();
    assert_eq!(
        world.claim_check(client, &urls[0]).unwrap(),
        ClaimOutcome::Received(usdt(dec!(10)))
    );
    let notice = one(&mut rx);
    assert_eq!(notice.chat, CB_CHAT);
    assert_eq!(
        notice.text,
        format!(
            "Ваш чек активировал @client.\n\nЧек на 10 USDT — {}",
            urls[0]
        )
    );
    assert_eq!(world.balance(client, CB, Asset::Usdt), dec!(10));
}

#[tokio::test]
async fn inline_xrocket_offers_every_asset() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, XR, Asset::Usdt, dec!(50)).unwrap();
    world.set_balance(ub, XR, Asset::Ton, dec!(50)).unwrap();
    let results = t.inline_query(XR, "10").await.unwrap();
    let titles: Vec<_> = results
        .results
        .iter()
        .map(|r| r.title.clone().unwrap())
        .collect();
    assert_eq!(titles, vec!["Чек на 10 USDT", "Чек на 10 TONCOIN"]);
    let sent = t
        .send_inline(Chat::SavedMessages, XR, &results, "check-TON", 1)
        .await
        .unwrap();
    assert_eq!(sent.text, "🚀 Чек на 10 TONCOIN");
    assert_eq!(sent.via_bot, Some(XR));
    assert!(sent.button_urls()[0].starts_with("https://t.me/xrocket?start=t_"));
    assert_eq!(world.balance(ub, XR, Asset::Ton), dec!(40));
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(50));
    assert_eq!(world.checks()[0].amount, ton(dec!(10)));
}

#[tokio::test]
async fn inline_insufficient_funds_creates_nothing() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, CB, Asset::Usdt, dec!(5)).unwrap();
    let results = t.inline_query(CB, "10 usdt").await.unwrap();
    assert_eq!(
        results.results[0].title.as_deref(),
        Some(texts::INLINE_INSUFFICIENT_TITLE)
    );
    let sent = t
        .send_inline(Chat::SavedMessages, CB, &results, "check-USDT", 1)
        .await
        .unwrap();
    assert_eq!(sent.text, texts::INLINE_INSUFFICIENT_MESSAGE);
    assert!(sent.buttons.is_empty());

    // Денег хватало при запросе, но не при отправке: чек не создан.
    world.set_balance(ub, CB, Asset::Usdt, dec!(20)).unwrap();
    let results = t.inline_query(CB, "10usdt").await.unwrap();
    world.set_balance(ub, CB, Asset::Usdt, dec!(3)).unwrap();
    let sent = t
        .send_inline(Chat::SavedMessages, CB, &results, "check-USDT", 2)
        .await
        .unwrap();
    assert_eq!(sent.text, texts::INLINE_INSUFFICIENT_MESSAGE);
    assert!(world.checks().is_empty());
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(3));
}

#[tokio::test]
async fn inline_bad_query_or_result_does_not_consume_random_id() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, CB, Asset::Usdt, dec!(50)).unwrap();
    let results = t.inline_query(CB, "10usdt").await.unwrap();
    let err = t
        .send_inline(Chat::SavedMessages, XR, &results, "check-USDT", 9)
        .await
        .unwrap_err();
    assert!(matches!(err, TransportError::Rpc { ref name, .. } if name == "QUERY_ID_INVALID"));
    let err = t
        .send_inline(Chat::SavedMessages, CB, &results, "nope", 9)
        .await
        .unwrap_err();
    assert!(matches!(err, TransportError::Rpc { ref name, .. } if name == "RESULT_ID_INVALID"));
    // Чужой запрос: query_id принадлежит другому аккаунту.
    let other = world.transport(world.add_account("other"));
    assert!(
        other
            .send_inline(Chat::SavedMessages, CB, &results, "check-USDT", 1)
            .await
            .is_err()
    );
    t.send_inline(Chat::SavedMessages, CB, &results, "check-USDT", 9)
        .await
        .unwrap();
    assert_eq!(world.checks().len(), 1);
    assert!(
        t.inline_query(CB, "10btc")
            .await
            .unwrap()
            .results
            .is_empty()
    );
}

#[tokio::test]
async fn inline_invoice_result_creates_our_invoice() {
    let Env { world, ub, t, .. } = env();
    let results = t.inline_query(CB, "3usdt").await.unwrap();
    let sent = t
        .send_inline(Chat::SavedMessages, CB, &results, "invoice-USDT", 1)
        .await
        .unwrap();
    assert_eq!(sent.text, "🧾 Счёт на 3 USDT");
    let inv = world.invoices().pop().unwrap();
    assert_eq!(inv.creator, Some(ub));
    assert_eq!(sent.button_urls(), vec![inv.url.clone()]);
    assert!(inv.code.starts_with("IV") && inv.code.len() == 12);
}

// ---------- сбои ----------

#[tokio::test]
async fn timeout_after_effect_really_created_the_inline_check() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, CB, Asset::Usdt, dec!(50)).unwrap();
    let results = t.inline_query(CB, "10usdt").await.unwrap();
    world.inject(ub, Call::SendInline, Fault::TimeoutAfterEffect, 1);
    let err = t
        .send_inline(Chat::SavedMessages, CB, &results, "check-USDT", 77)
        .await
        .unwrap_err();
    assert_eq!(err, TransportError::Timeout);
    assert!(err.outcome_unknown());
    let checks = world.checks_created_by(ub);
    assert_eq!(checks.len(), 1, "чек создан, хотя вызов вернул таймаут");
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(40));
    // Сверка: сообщение со ссылкой есть в «Избранном».
    let saved = t.history(Chat::SavedMessages, 0, 10).await.unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].via_bot, Some(CB));
    assert_eq!(saved[0].button_urls(), vec![checks[0].url.clone()]);
    // Повтор с тем же random_id второго чека не создаёт.
    assert_eq!(
        t.send_inline(Chat::SavedMessages, CB, &results, "check-USDT", 77)
            .await
            .unwrap_err(),
        TransportError::RandomIdDuplicate
    );
    assert_eq!(world.checks().len(), 1);
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(40));
}

#[tokio::test]
async fn timeout_after_effect_in_menu_still_delivers_the_link() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, CB, Asset::Usdt, dec!(50)).unwrap();
    let mut rx = t.subscribe();
    t.send_text(CB_CHAT, "/checks", 1).await.unwrap();
    let menu = one(&mut rx);
    t.press(CB_CHAT, menu.id, &data(&menu, "Создать чек"))
        .await
        .unwrap();
    let a = one(&mut rx);
    t.press(CB_CHAT, menu.id, &data(&a, "USDT")).await.unwrap();
    one(&mut rx);
    world.inject(ub, Call::SendText, Fault::TimeoutAfterEffect, 1);
    assert_eq!(
        t.send_text(CB_CHAT, "10", 2).await.unwrap_err(),
        TransportError::Timeout
    );
    let reply = one(&mut rx);
    let check = world.checks_created_by(ub).pop().unwrap();
    assert!(reply.text.contains(&check.url));
    // Наше сообщение с суммой есть в истории, хотя id его не вернулся.
    let hist = t.history(CB_CHAT, 0, 100).await.unwrap();
    assert!(hist.iter().any(|m| m.out && m.text == "10"));
    assert_eq!(
        t.send_text(CB_CHAT, "10", 2).await.unwrap_err(),
        TransportError::RandomIdDuplicate
    );
    assert_eq!(world.checks().len(), 1);
}

#[tokio::test]
async fn failures_before_effect_do_nothing_and_keep_random_id_free() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, CB, Asset::Usdt, dec!(50)).unwrap();
    let results = t.inline_query(CB, "10usdt").await.unwrap();
    for fault in [
        Fault::TimeoutBeforeEffect,
        Fault::Disconnected,
        Fault::FloodWait(Duration::from_secs(29)),
        Fault::Rpc {
            code: 400,
            name: "PEER_ID_INVALID".into(),
        },
    ] {
        world.inject(ub, Call::SendInline, fault.clone(), 1);
        let err = t
            .send_inline(Chat::SavedMessages, CB, &results, "check-USDT", 5)
            .await
            .unwrap_err();
        match fault {
            Fault::TimeoutBeforeEffect => assert_eq!(err, TransportError::Timeout),
            Fault::Disconnected => assert_eq!(err, TransportError::Disconnected),
            Fault::FloodWait(d) => assert_eq!(err, TransportError::FloodWait(d)),
            _ => assert!(matches!(err, TransportError::Rpc { code: 400, .. })),
        }
        assert!(world.checks().is_empty(), "{fault:?}");
        assert!(world.messages(ub, Chat::SavedMessages).is_empty());
        assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(50));
    }
    // random_id не занят: та же отправка проходит один раз.
    t.send_inline(Chat::SavedMessages, CB, &results, "check-USDT", 5)
        .await
        .unwrap();
    assert_eq!(world.checks().len(), 1);
}

#[tokio::test]
async fn disconnect_after_effect_on_start_still_claims() {
    let Env { world, ub, t, .. } = env();
    let url = world.client_check(CB, usdt(dec!(4))).unwrap();
    let mut rx = t.subscribe();
    world.inject(ub, Call::SendText, Fault::DisconnectedAfterEffect, 1);
    let err = t
        .send_text(CB_CHAT, &format!("/start {}", start_param(&url)), 1)
        .await
        .unwrap_err();
    assert_eq!(err, TransportError::Disconnected);
    assert_eq!(one(&mut rx).text, "Вы получили 4 USDT.");
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(4));
}

#[tokio::test]
async fn faults_apply_to_next_n_matching_calls_only() {
    let Env { world, ub, t, .. } = env();
    world.inject(ub, Call::Any, Fault::Disconnected, 2);
    world.inject(ub, Call::History, Fault::TimeoutBeforeEffect, 1);
    assert_eq!(
        t.inline_query(CB, "1").await.unwrap_err(),
        TransportError::Disconnected
    );
    assert_eq!(
        t.send_text(CB_CHAT, "/wallet", 1).await.unwrap_err(),
        TransportError::Disconnected
    );
    // Правило Any исчерпано, дальше — правило для history.
    assert_eq!(
        t.history(CB_CHAT, 0, 10).await.unwrap_err(),
        TransportError::Timeout
    );
    assert!(t.history(CB_CHAT, 0, 10).await.unwrap().is_empty());
    t.send_text(CB_CHAT, "/wallet", 1).await.unwrap();
    world.inject(ub, Call::Press, Fault::FloodWait(Duration::from_secs(3)), 5);
    world.clear_faults(ub);
    t.send_text(CB_CHAT, "/wallet", 2).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn delayed_reply_arrives_later_but_effect_is_immediate() {
    let Env { world, ub, t, .. } = env();
    let url = world.client_check(CB, usdt(dec!(2))).unwrap();
    let mut rx = t.subscribe();
    world.inject(
        ub,
        Call::SendText,
        Fault::DelayReply(Duration::from_secs(5)),
        1,
    );
    let sent = start(&t, CB_CHAT, start_param(&url), 1).await;
    assert!(drain(&mut rx).is_empty());
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(2));
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(drain(&mut rx).is_empty());
    let reply = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reply.text, "Вы получили 2 USDT.");
    assert!(reply.id > sent.id);
}

#[tokio::test(start_paused = true)]
async fn processing_message_is_edited_into_the_final_reply() {
    let Env { world, ub, t, .. } = env();
    let url = world.client_check(XR, usdt(dec!(3))).unwrap();
    let mut rx = t.subscribe();
    world.inject(
        ub,
        Call::SendText,
        Fault::ProcessingFirst {
            final_after: Duration::from_secs(2),
        },
        1,
    );
    start(&t, XR_CHAT, start_param(&url), 1).await;
    let first = one(&mut rx);
    assert_eq!(first.text, texts::PROCESSING);
    assert_eq!(first.edit_date, None);
    let fin = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fin.id, first.id, "правка того же сообщения");
    assert!(fin.edit_date.is_some());
    assert_eq!(fin.text, "🚀 Вы получили 3 USDT");
    let hist = t.history(XR_CHAT, 0, 10).await.unwrap();
    assert_eq!(hist.last().unwrap().text, "🚀 Вы получили 3 USDT");

    // Без паузы обе версии приходят сразу.
    let url = world.client_check(XR, usdt(dec!(1))).unwrap();
    world.inject(
        ub,
        Call::SendText,
        Fault::ProcessingFirst {
            final_after: Duration::ZERO,
        },
        1,
    );
    start(&t, XR_CHAT, start_param(&url), 2).await;
    let both = drain(&mut rx);
    assert_eq!(both.len(), 2);
    assert_eq!(both[0].text, texts::PROCESSING);
    assert_eq!(both[1].id, both[0].id);
    assert_eq!(both[1].text, "🚀 Вы получили 1 USDT");
}

#[tokio::test]
async fn unknown_text_no_reply_and_edit_instead_of_new() {
    let Env { world, ub, t, .. } = env();
    let mut rx = t.subscribe();

    let url = world.client_check(CB, usdt(dec!(1))).unwrap();
    world.inject(
        ub,
        Call::SendText,
        Fault::UnknownReply(texts::UNKNOWN_DEFAULT.into()),
        1,
    );
    start(&t, CB_CHAT, start_param(&url), 1).await;
    assert_eq!(one(&mut rx).text, texts::UNKNOWN_DEFAULT);
    assert_eq!(
        world.balance(ub, CB, Asset::Usdt),
        dec!(1),
        "действие выполнено"
    );

    let url = world.client_check(CB, usdt(dec!(2))).unwrap();
    world.inject(ub, Call::SendText, Fault::NoReply, 1);
    start(&t, CB_CHAT, start_param(&url), 2).await;
    assert!(drain(&mut rx).is_empty());
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(3));

    t.send_text(CB_CHAT, "/wallet", 3).await.unwrap();
    let wallet = one(&mut rx);
    let url = world.client_check(CB, usdt(dec!(4))).unwrap();
    world.inject(ub, Call::SendText, Fault::EditInsteadOfNew, 1);
    let sent = start(&t, CB_CHAT, start_param(&url), 4).await;
    let edited = one(&mut rx);
    assert_eq!(edited.id, wallet.id);
    assert!(edited.id < sent.id, "ответ — правка старого сообщения");
    assert!(edited.edit_date.is_some());
    assert_eq!(edited.text, "Вы получили 4 USDT.");
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(7));
}

// ---------- счета ----------

#[tokio::test]
async fn cryptobot_invoice_is_paid_through_webapp_pin() {
    let Env { world, ub, t, .. } = env();
    let shop = world.add_account("shop");
    let inv = world
        .create_invoice(
            InvoiceSpec::crypto(CB, usdt(dec!(10)))
                .by(shop)
                .description("Заказ 42"),
        )
        .unwrap();
    assert!(inv.code.starts_with("IV") && inv.code.len() == 12);
    assert_eq!(inv.url, format!("https://t.me/send?start={}", inv.code));
    world.set_balance(ub, CB, Asset::Usdt, dec!(25)).unwrap();
    world.set_pin(ub, "1234").unwrap();
    let mut rx = t.subscribe();

    start(&t, CB_CHAT, &inv.code, 1).await;
    let card = one(&mut rx);
    for part in [
        format!("Счёт {}", inv.code),
        "Сумма: 10 USDT".into(),
        "Описание: Заказ 42".into(),
        "Одноразовый счёт".into(),
        "Статус: ожидает оплаты".into(),
    ] {
        assert!(card.text.contains(&part), "{part} в {}", card.text);
    }
    t.press(CB_CHAT, card.id, &data(&card, "USDT"))
        .await
        .unwrap();
    let screen = one(&mut rx);
    assert_eq!(screen.id, card.id);
    assert!(screen.text.contains("К оплате: 10 USDT"));
    let pay = screen.find_button("Оплатить").unwrap();
    let ButtonKind::WebApp(button_url) = &pay.kind else {
        panic!("у CryptoBot «Оплатить» — мини-приложение: {pay:?}");
    };
    // Нажать её как callback нельзя.
    assert!(t.press(CB_CHAT, card.id, b"pay").await.is_err());

    let url = t.open_webapp(CB, button_url).await.unwrap();
    assert!(url.contains("session="));
    assert_eq!(
        world.complete_webapp_payment(&url, "0000").unwrap(),
        WebAppPayment::WrongPin { attempts_left: 2 }
    );
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(25));
    assert_eq!(
        world.invoice(&inv.code).unwrap().status,
        SimInvoiceStatus::Active
    );

    assert_eq!(
        world.complete_webapp_payment(&url, "1234").unwrap(),
        WebAppPayment::Paid(usdt(dec!(10)))
    );
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(15));
    assert_eq!(world.balance(shop, CB, Asset::Usdt), dec!(10));
    let paid = world.invoice(&inv.code).unwrap();
    assert_eq!(paid.status, SimInvoiceStatus::Paid);
    assert_eq!(paid.payments.len(), 1);
    assert_eq!(
        one(&mut rx).text,
        format!("✅ Счёт {} оплачен.\n\nСписано: 10 USDT.", inv.code)
    );
    assert!(
        world
            .messages(shop, CB_CHAT)
            .pop()
            .unwrap()
            .text
            .contains("оплачен пользователем @exch_cb")
    );

    // Повторная отправка формы и новая сессия деньги не трогают.
    assert_eq!(
        world.complete_webapp_payment(&url, "1234").unwrap(),
        WebAppPayment::SessionUsed
    );
    let again = t.open_webapp(CB, button_url).await.unwrap();
    assert_eq!(
        world.complete_webapp_payment(&again, "1234").unwrap(),
        WebAppPayment::AlreadyPaid
    );
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(15));

    start(&t, CB_CHAT, &inv.code, 2).await;
    let card = one(&mut rx);
    assert!(card.text.contains("Статус: оплачен"));
    assert!(card.buttons.is_empty());
}

#[tokio::test]
async fn webapp_pin_blocks_after_three_wrong_attempts_and_checks_funds() {
    let Env { world, ub, t, .. } = env();
    let inv = world
        .create_invoice(InvoiceSpec::crypto(CB, usdt(dec!(10))))
        .unwrap();
    world.set_pin(ub, "1234").unwrap();
    world.set_balance(ub, CB, Asset::Usdt, dec!(4)).unwrap();
    let button = format!(
        "https://pay.send.sim.invalid/invoice?code={}&asset=USDT",
        inv.code
    );
    let url = t.open_webapp(CB, &button).await.unwrap();
    assert_eq!(
        world.complete_webapp_payment(&url, "1234").unwrap(),
        WebAppPayment::InsufficientFunds {
            available: usdt(dec!(4)),
            needed: usdt(dec!(10))
        }
    );
    for left in [2, 1] {
        assert_eq!(
            world.complete_webapp_payment(&url, "1").unwrap(),
            WebAppPayment::WrongPin {
                attempts_left: left
            }
        );
    }
    assert_eq!(
        world.complete_webapp_payment(&url, "1").unwrap(),
        WebAppPayment::Blocked
    );
    assert_eq!(
        world.complete_webapp_payment(&url, "1234").unwrap(),
        WebAppPayment::SessionUsed
    );
    assert!(
        world
            .complete_webapp_payment("https://example.com/?session=1", "1234")
            .is_err()
    );
    assert!(t.open_webapp(XR, &button).await.is_err());
    assert!(
        t.open_webapp(CB, "https://evil.example/invoice?code=IV&asset=USDT")
            .await
            .is_err()
    );
    // Таймаут после открытия: сессия есть, но URL потерян.
    world.inject(ub, Call::OpenWebApp, Fault::TimeoutAfterEffect, 1);
    assert_eq!(
        t.open_webapp(CB, &button).await.unwrap_err(),
        TransportError::Timeout
    );
    world.expire_invoice(&inv.code).unwrap();
    let url = t.open_webapp(CB, &button).await.unwrap();
    assert_eq!(
        world.complete_webapp_payment(&url, "1234").unwrap(),
        WebAppPayment::Expired
    );
}

#[tokio::test]
async fn fiat_invoice_shows_amount_in_asset_by_rate() {
    let Env { world, ub, t, .. } = env();
    world.set_fiat_rate(Asset::Usdt, "EUR", dec!(0.95));
    let inv = world
        .create_invoice(InvoiceSpec::fiat(
            CB,
            dec!(10),
            "EUR",
            &[Asset::Usdt, Asset::Ton],
        ))
        .unwrap();
    world.set_balance(ub, CB, Asset::Usdt, dec!(100)).unwrap();
    let mut rx = t.subscribe();
    start(&t, CB_CHAT, &inv.code, 1).await;
    let card = one(&mut rx);
    assert!(card.text.contains("Сумма: 10 EUR"));
    assert!(card.find_button("USDT").is_some());
    assert!(card.find_button("TON").is_none(), "курса TON/EUR нет");
    t.press(CB_CHAT, card.id, &data(&card, "USDT"))
        .await
        .unwrap();
    let screen = one(&mut rx);
    // 10 / 0.95 = 10.526… → вверх до 0.01.
    assert!(
        screen.text.contains("К оплате: 10.53 USDT"),
        "{}",
        screen.text
    );
    let ButtonKind::WebApp(button) = &screen.find_button("Оплатить").unwrap().kind else {
        panic!()
    };
    let url = t.open_webapp(CB, button).await.unwrap();
    assert_eq!(
        world.complete_webapp_payment(&url, "any").unwrap(),
        WebAppPayment::Paid(usdt(dec!(10.53)))
    );
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(89.47));
}

#[tokio::test]
async fn xrocket_invoice_is_paid_by_button() {
    let Env {
        world,
        ub,
        t,
        client,
    } = env();
    let inv = world
        .create_invoice(InvoiceSpec::crypto(XR, usdt(dec!(10))).by(client))
        .unwrap();
    assert!(inv.code.starts_with("inv_"));
    assert_eq!(inv.url, format!("https://t.me/xrocket?start={}", inv.code));
    world.set_balance(ub, XR, Asset::Usdt, dec!(20)).unwrap();
    let mut rx = t.subscribe();
    start(&t, XR_CHAT, &inv.code, 1).await;
    let card = one(&mut rx);
    t.press(XR_CHAT, card.id, &data(&card, "USDT"))
        .await
        .unwrap();
    let screen = one(&mut rx);
    assert!(screen.text.contains("К оплате: 10 USDT"));
    t.press(XR_CHAT, card.id, &data(&screen, "Оплатить"))
        .await
        .unwrap();
    let done = one(&mut rx);
    assert_eq!(done.id, card.id);
    assert_eq!(
        done.text,
        format!("✅ Счёт {} оплачен.\n\nСписано: 10 USDT.", inv.code)
    );
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(10));
    assert_eq!(world.balance(client, XR, Asset::Usdt), dec!(10));
    assert_eq!(
        world.invoice(&inv.code).unwrap().status,
        SimInvoiceStatus::Paid
    );
    // Кнопки больше нет — повторное нажатие отклонено, денег не тронуто.
    let err = t
        .press(XR_CHAT, card.id, &data(&screen, "Оплатить"))
        .await
        .unwrap_err();
    assert!(matches!(err, TransportError::Rpc { ref name, .. } if name == "DATA_INVALID"));
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(10));
    start(&t, XR_CHAT, &inv.code, 2).await;
    assert!(one(&mut rx).text.contains("Статус: оплачен"));
    start(&t, XR_CHAT, "inv_AAAAAAAAAAAAAAA", 3).await;
    assert_eq!(one(&mut rx).text, texts::INVOICE_NOT_FOUND);
}

#[tokio::test]
async fn xrocket_invoice_statuses_and_timeout_after_press() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, XR, Asset::Usdt, dec!(5)).unwrap();
    let mut rx = t.subscribe();
    let open_pay_screen = |code: String, rid: i64| {
        let t = t.clone();
        async move {
            let mut rx = t.subscribe();
            start(&t, XR_CHAT, &code, rid).await;
            let card = one(&mut rx);
            t.press(XR_CHAT, card.id, &data(&card, "USDT"))
                .await
                .unwrap();
            (card.id, one(&mut rx))
        }
    };

    // Не хватает денег.
    let inv = world
        .create_invoice(InvoiceSpec::crypto(XR, usdt(dec!(10))))
        .unwrap();
    let (id, screen) = open_pay_screen(inv.code.clone(), 1).await;
    t.press(XR_CHAT, id, &data(&screen, "Оплатить"))
        .await
        .unwrap();
    let last = drain(&mut rx).pop().unwrap();
    assert_eq!(
        last.text,
        "Недостаточно средств для оплаты счёта. Доступно: 5 USDT, нужно 10 USDT."
    );
    assert_eq!(
        world.invoice(&inv.code).unwrap().status,
        SimInvoiceStatus::Active
    );

    // Истёк между экраном и нажатием.
    let inv = world
        .create_invoice(InvoiceSpec::crypto(XR, usdt(dec!(1))))
        .unwrap();
    let (id, screen) = open_pay_screen(inv.code.clone(), 2).await;
    world.expire_invoice(&inv.code).unwrap();
    t.press(XR_CHAT, id, &data(&screen, "Оплатить"))
        .await
        .unwrap();
    assert_eq!(drain(&mut rx).pop().unwrap().text, texts::INVOICE_EXPIRED);
    start(&t, XR_CHAT, &inv.code, 3).await;
    let card = drain(&mut rx).pop().unwrap();
    assert!(card.text.contains("Статус: истёк"));
    assert!(card.buttons.is_empty());

    // Многоразовый: платится дважды, остаётся активным.
    let inv = world
        .create_invoice(InvoiceSpec::crypto(XR, usdt(dec!(1))).multi_use())
        .unwrap();
    for rid in [4, 6] {
        let (id, screen) = open_pay_screen(inv.code.clone(), rid).await;
        assert!(screen.text.contains("Многоразовый счёт"));
        t.press(XR_CHAT, id, &data(&screen, "Оплатить"))
            .await
            .unwrap();
    }
    let multi = world.invoice(&inv.code).unwrap();
    assert_eq!(multi.status, SimInvoiceStatus::Active);
    assert_eq!(multi.payments.len(), 2);
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(3));

    // Таймаут после нажатия «Оплатить»: оплачено.
    let inv = world
        .create_invoice(InvoiceSpec::crypto(XR, usdt(dec!(2))))
        .unwrap();
    let (id, screen) = open_pay_screen(inv.code.clone(), 8).await;
    world.inject(ub, Call::Press, Fault::TimeoutAfterEffect, 1);
    assert_eq!(
        t.press(XR_CHAT, id, &data(&screen, "Оплатить"))
            .await
            .unwrap_err(),
        TransportError::Timeout
    );
    assert_eq!(
        world.invoice(&inv.code).unwrap().status,
        SimInvoiceStatus::Paid
    );
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(1));
    assert!(drain(&mut rx).pop().unwrap().text.contains("оплачен"));
}

// ---------- кошелёк, уведомления, история ----------

#[tokio::test]
async fn wallet_screen_lists_balances() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, CB, Asset::Usdt, dec!(12.50)).unwrap();
    world.set_balance(ub, XR, Asset::Ton, dec!(1.5)).unwrap();
    let mut rx = t.subscribe();
    t.send_text(CB_CHAT, "/wallet", 1).await.unwrap();
    assert_eq!(
        one(&mut rx).text,
        "👛 Кошелёк\n\nTether: 12.5 USDT\nToncoin: 0 TON"
    );
    t.send_text(XR_CHAT, "👛 Кошелёк", 2).await.unwrap();
    assert_eq!(
        one(&mut rx).text,
        "👛 Мой кошелёк\n\nTether: 0 USDT\nToncoin: 1.5 TONCOIN"
    );
    assert_eq!(world.balances(ub, XR), vec![usdt(dec!(0)), ton(dec!(1.5))]);
    t.send_text(CB_CHAT, "/start", 3).await.unwrap();
    assert_eq!(one(&mut rx).text, texts::welcome(CB));
    t.send_text(CB_CHAT, "привет", 4).await.unwrap();
    assert_eq!(one(&mut rx).text, texts::UNKNOWN_COMMAND);
}

#[tokio::test]
async fn incoming_transfer_notice_credits_balance() {
    let Env { world, ub, t, .. } = env();
    let mut rx = t.subscribe();
    world.incoming_transfer(ub, CB, usdt(dec!(5)), "x").unwrap();
    let msg = one(&mut rx);
    assert_eq!(msg.text, "Вы получили 5 USDT от @x.");
    assert_eq!(msg.chat, CB_CHAT);
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(5));
    let ad = world.bot_message(ub, XR, "Реклама").unwrap();
    assert_eq!(one(&mut rx), ad);
}

#[tokio::test]
async fn history_returns_messages_after_id_in_order_with_edits() {
    let Env { world, ub, t, .. } = env();
    let mut rx = t.subscribe();
    let first = t.send_text(CB_CHAT, "/checks", 1).await.unwrap();
    let menu = one(&mut rx);
    t.send_text(Chat::SavedMessages, "заметка", 2)
        .await
        .unwrap();
    t.send_text(CB_CHAT, "/wallet", 3).await.unwrap();
    t.press(CB_CHAT, menu.id, &data(&menu, "Создать чек"))
        .await
        .unwrap();

    let all = t.history(CB_CHAT, 0, 100).await.unwrap();
    assert_eq!(all.len(), 4);
    assert!(all.windows(2).all(|w| w[0].id < w[1].id));
    assert_eq!(all[0], first);
    assert_eq!(
        all[1].text,
        texts::CHOOSE_CHECK_ASSET,
        "в истории — последняя версия"
    );
    let after = t.history(CB_CHAT, menu.id, 1).await.unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].text, "/wallet");
    assert!(t.history(CB_CHAT, all[3].id, 10).await.unwrap().is_empty());
    let saved = t.history(Chat::SavedMessages, 0, 10).await.unwrap();
    assert_eq!(saved.len(), 1);
    assert!(saved[0].out);
    assert_eq!(world.messages(ub, CB_CHAT), all);
}

#[tokio::test]
async fn press_rejects_unknown_message_or_data() {
    let Env { t, .. } = env();
    let mut rx = t.subscribe();
    let sent = t.send_text(CB_CHAT, "/checks", 1).await.unwrap();
    let menu = one(&mut rx);
    let name = |e: TransportError| match e {
        TransportError::Rpc { name, .. } => name,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        name(t.press(CB_CHAT, 999_999, b"x").await.unwrap_err()),
        "MESSAGE_ID_INVALID"
    );
    assert_eq!(
        name(
            t.press(CB_CHAT, menu.id, b"cb:checks:asset:USDT")
                .await
                .unwrap_err()
        ),
        "DATA_INVALID"
    );
    assert_eq!(
        name(
            t.press(CB_CHAT, sent.id, b"cb:checks:create")
                .await
                .unwrap_err()
        ),
        "DATA_INVALID"
    );
    assert_eq!(
        name(
            t.press(XR_CHAT, menu.id, b"cb:checks:create")
                .await
                .unwrap_err()
        ),
        "MESSAGE_ID_INVALID"
    );
}

#[tokio::test]
async fn many_accounts_share_one_world_and_money_is_conserved() {
    let world = SimWorld::new();
    let cb_bot = world.add_account("exch_cb");
    let xr_bot = world.add_account("exch_xr");
    let alice = world.add_account("alice");
    let bob = world.add_account("bob");
    let (t_cb, t_xr) = (world.transport(cb_bot), world.transport(xr_bot));
    world
        .set_balance(alice, CB, Asset::Usdt, dec!(100))
        .unwrap();
    world
        .set_balance(xr_bot, XR, Asset::Usdt, dec!(100))
        .unwrap();
    let cb_total = world.total_in_world(CB, Asset::Usdt);
    let xr_total = world.total_in_world(XR, Asset::Usdt);

    // Алиса отдаёт чек CryptoBot, сервис принимает его одним аккаунтом…
    let check = world
        .create_check(CheckSpec::new(CB, usdt(dec!(40))).by(alice))
        .unwrap();
    start(&t_cb, CB_CHAT, &check.code, 1).await;
    // …и выплачивает чеком xRocket с другого аккаунта, Боб его забирает.
    let results = t_xr.inline_query(XR, "39").await.unwrap();
    let msg = t_xr
        .send_inline(Chat::SavedMessages, XR, &results, "check-USDT", 1)
        .await
        .unwrap();
    assert_eq!(
        world.claim_check(bob, &msg.button_urls()[0]).unwrap(),
        ClaimOutcome::Received(usdt(dec!(39)))
    );

    assert_eq!(world.balance(cb_bot, CB, Asset::Usdt), dec!(40));
    assert_eq!(world.balance(xr_bot, XR, Asset::Usdt), dec!(61));
    assert_eq!(world.balance(bob, XR, Asset::Usdt), dec!(39));
    assert_eq!(world.total_in_world(CB, Asset::Usdt), cb_total);
    assert_eq!(world.total_in_world(XR, Asset::Usdt), xr_total);
    // У каждого аккаунта свой поток и свои random_id.
    assert!(world.messages(cb_bot, XR_CHAT).is_empty());
    assert!(t_xr.history(CB_CHAT, 0, 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn cancelled_check_refunds_creator_and_is_not_found() {
    let Env {
        world,
        ub,
        t,
        client,
    } = env();
    world
        .set_balance(client, CB, Asset::Usdt, dec!(10))
        .unwrap();
    let check = world
        .create_check(CheckSpec::new(CB, usdt(dec!(3))).by(client).multi(2))
        .unwrap();
    assert_eq!(world.balance(client, CB, Asset::Usdt), dec!(4));
    world.cancel_check(&check.code).unwrap();
    assert_eq!(world.balance(client, CB, Asset::Usdt), dec!(10));
    let mut rx = t.subscribe();
    start(&t, CB_CHAT, &check.code, 1).await;
    assert_eq!(one(&mut rx).text, texts::CHECK_NOT_FOUND);
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(0));
    assert!(
        world
            .create_check(CheckSpec::new(CB, usdt(dec!(100))).by(client))
            .is_err()
    );
    assert!(
        world
            .create_check(CheckSpec::new(XR, usdt(dec!(1))).with_password("x"))
            .is_err()
    );
}

#[tokio::test]
async fn worlds_with_one_seed_are_identical_and_unknown_account_is_unauthorized() {
    let codes = |seed| {
        let w = SimWorld::with_seed(seed);
        (0..5)
            .map(|_| w.client_check(CB, usdt(dec!(1))).unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(codes(1), codes(1));
    assert_ne!(codes(1), codes(2));

    let world = SimWorld::new();
    let ghost = world.transport(AccountId(1));
    assert_eq!(
        ghost.send_text(CB_CHAT, "/start", 1).await.unwrap_err(),
        TransportError::NotAuthorized
    );
    assert_eq!(
        ghost.history(CB_CHAT, 0, 1).await.unwrap_err(),
        TransportError::NotAuthorized
    );
}
