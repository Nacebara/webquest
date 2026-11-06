//! Симулятор ботов кошельков (DESIGN-v0.2 §5) на живых экранах: активация, меню чеков обоих
//! кошельков, счета, кошелёк, сбои. Ключевые экраны дополнительно прогоняются через парсеры —
//! симулятор и разбор не должны разойтись.
#![cfg(feature = "sim")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use domain::{Asset, Decimal, Money, Platform};
use parsers::ActivationReply;
use parsers::replies::{classify_activation, parse_balance};
use parsers::screens::{
    CheckCreationReply, InvoiceScreen, classify_check_creation, classify_invoice_screen,
};
use rust_decimal_macros::dec;
use tokio::sync::broadcast;
use userbot::sim::{
    AccountId, Call, CheckOrigin, CheckSpec, CheckStatus, ClaimOutcome, Fault, InvoiceSpec,
    SimInvoiceStatus, SimTransport, SimWorld, texts,
};
use userbot::transport::{ButtonKind, Chat, RawMessage, Transport, TransportError};

const CB: Platform = Platform::CryptoBot;
const XR: Platform = Platform::XRocket;
const CB_CHAT: Chat = Chat::WalletBot(Platform::CryptoBot);
const XR_CHAT: Chat = Chat::WalletBot(Platform::XRocket);
const NBSP: char = '\u{a0}';

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

/// Последние версии сообщений из потока, по порядку первого появления.
fn settled(rx: &mut broadcast::Receiver<RawMessage>) -> Vec<RawMessage> {
    let mut out: Vec<RawMessage> = Vec::new();
    for m in drain(rx) {
        match out.iter_mut().find(|o| o.id == m.id && o.chat == m.chat) {
            Some(o) => *o = m,
            None => out.push(m),
        }
    }
    out
}

/// Ровно одно сообщение бота (последняя версия, если бот его правил).
fn one(rx: &mut broadcast::Receiver<RawMessage>) -> RawMessage {
    let mut msgs = settled(rx);
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

async fn press(t: &SimTransport, chat: Chat, msg: &RawMessage, button: &str) {
    let answer = t.press(chat, msg.id, &data(msg, button)).await.unwrap();
    assert!(!answer.alert, "{button}: {answer:?}");
}

fn start_param(url: &str) -> &str {
    url.split("start=").nth(1).unwrap()
}

async fn start(t: &SimTransport, chat: Chat, code: &str, rid: i64) -> RawMessage {
    t.send_text(chat, &format!("/start {code}"), rid)
        .await
        .unwrap()
}

fn urls(m: &RawMessage) -> Vec<String> {
    let mut u = m.button_urls();
    u.extend(m.entity_urls.iter().cloned());
    u
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
    assert!(check.code.starts_with("CQ") && check.code.len() == 12);
    assert_eq!(check.url, format!("https://t.me/send?start={}", check.code));
    let total = world.total_in_world(CB, Asset::Usdt);

    let mut rx = t.subscribe();
    let sent = start(&t, CB_CHAT, &check.code, 1).await;
    assert!(sent.out);
    // «🕘 Получение…», затем правка того же сообщения.
    let versions = drain(&mut rx);
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].text, "🕘 Получение 🪙 100 USDT ($100)…");
    assert_eq!(versions[1].id, versions[0].id);
    assert!(versions[1].edit_date.is_some());
    assert_eq!(versions[1].text, "📥 Вы получили 🪙 100 USDT ($100).");
    assert!(versions[1].id > sent.id);
    assert_eq!(
        classify_activation(CB, &versions[0].text),
        ActivationReply::InProgress
    );
    assert_eq!(
        classify_activation(CB, &versions[1].text),
        ActivationReply::Received {
            amount: dec!(100),
            asset: Asset::Usdt
        }
    );

    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(100));
    let check = world.check(&check.code).unwrap();
    assert_eq!(check.status, CheckStatus::Claimed);
    assert_eq!(check.claimed_by, vec![ub]);
    assert_eq!(world.total_in_world(CB, Asset::Usdt), total);

    let notice = world.messages(client, CB_CHAT).pop().unwrap();
    assert_eq!(
        notice.text,
        "🦋 \u{2068}@exch_cb\u{2069} активировал(а) ваш чек и получил(а) 🪙 100 USDT ($100)."
    );
    assert_eq!(
        classify_activation(CB, &notice.text),
        ActivationReply::ClaimNotice
    );
}

#[tokio::test]
async fn second_start_with_new_random_id_says_already_activated() {
    let Env { world, ub, t, .. } = env();
    let mut rx = t.subscribe();
    for (platform, chat, rid) in [(CB, CB_CHAT, 10), (XR, XR_CHAT, 20)] {
        let url = world.client_check(platform, usdt(dec!(5))).unwrap();
        let code = start_param(&url).to_owned();
        start(&t, chat, &code, rid + 1).await;
        assert!(matches!(
            classify_activation(platform, &one(&mut rx).text),
            ActivationReply::Received { .. }
        ));
        // Ловушка lovec (AUDIT.md:46): повтор с НОВЫМ random_id после выигрыша.
        start(&t, chat, &code, rid + 2).await;
        let again = one(&mut rx).text;
        match platform {
            CB => assert_eq!(again, "Этот чек уже активирован."),
            XR => {
                assert_eq!(again, "Этот перевод уже активирован вами.");
                assert_eq!(
                    classify_activation(XR, &again),
                    ActivationReply::AlreadyActivatedByUs
                );
            }
        }
        assert_eq!(world.balance(ub, platform, Asset::Usdt), dec!(5));
    }
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
    let cases: Vec<(Chat, String, &str, ActivationReply)> = vec![
        (
            CB_CHAT,
            "CQAAAAAAAAAA".into(),
            "Чек не найден, удалён или не был создан из-за недостаточного баланса.",
            ActivationReply::NotFound,
        ),
        (
            XR_CHAT,
            "t_AAAAAAAAAAAAAAA".into(),
            "Этот перевод не найден.",
            ActivationReply::NotFound,
        ),
        (
            XR_CHAT,
            "mci_AAAAAAAAAAAAAAA".into(),
            "Мульти-чек не найден.",
            ActivationReply::NotFound,
        ),
        (
            XR_CHAT,
            mk(CheckSpec::new(CB, m)),
            "Этот перевод не найден.",
            ActivationReply::NotFound,
        ),
        (
            CB_CHAT,
            mk(CheckSpec::new(CB, m).personal_for(other)),
            "К сожалению, вы не можете получить этот подарок. Он предназначен для другого получателя.",
            ActivationReply::NotForYou,
        ),
        (
            CB_CHAT,
            mk(CheckSpec::new(CB, m).with_subscription("@wallet_news")),
            "Подпишитесь на канал @wallet_news, чтобы активировать этот чек.",
            ActivationReply::NeedsSubscription,
        ),
        (
            CB_CHAT,
            mk(CheckSpec::new(CB, m).with_captcha()),
            texts::CAPTCHA,
            ActivationReply::Captcha,
        ),
        (
            CB_CHAT,
            mk(CheckSpec::new(CB, m).with_password("pw")),
            texts::PASSWORD_PROMPT,
            ActivationReply::PasswordRequired,
        ),
        (
            CB_CHAT,
            mk(CheckSpec::new(CB, m).premium_only()),
            texts::PREMIUM_ONLY,
            ActivationReply::PremiumOnly,
        ),
    ];
    let mut rx = t.subscribe();
    for (i, (chat, code, expected, class)) in cases.iter().enumerate() {
        start(&t, *chat, code, 100 + i64::try_from(i).unwrap()).await;
        let reply = one(&mut rx);
        assert_eq!(reply.text, *expected, "код {code}");
        let Chat::WalletBot(platform) = chat else {
            unreachable!()
        };
        assert_eq!(
            classify_activation(*platform, &reply.text),
            *class,
            "{code}"
        );
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
async fn subscription_premium_and_password_pass_when_satisfied() {
    let Env { world, ub, t, .. } = env();
    let m = usdt(dec!(2));
    let sub = world
        .create_check(CheckSpec::new(CB, m).with_subscription("@chan"))
        .unwrap();
    let prem = world
        .create_check(CheckSpec::new(XR, m).premium_only())
        .unwrap();
    let pw = world
        .create_check(CheckSpec::new(CB, usdt(dec!(1))).with_password("s3cret"))
        .unwrap();
    world.join_channel(ub, "@chan").unwrap();
    world.set_premium(ub, true).unwrap();
    let mut rx = t.subscribe();
    start(&t, CB_CHAT, &sub.code, 1).await;
    assert_eq!(one(&mut rx).text, "📥 Вы получили 🪙 2 USDT ($2).");
    start(&t, XR_CHAT, &prem.code, 2).await;
    assert_eq!(
        one(&mut rx).text,
        format!("💰 Вы успешно получили 2{NBSP}USDT (2.0$).")
    );
    start(&t, CB_CHAT, &pw.code, 3).await;
    assert_eq!(one(&mut rx).text, texts::PASSWORD_PROMPT);
    t.send_text(CB_CHAT, "wrong", 4).await.unwrap();
    assert_eq!(one(&mut rx).text, texts::WRONG_PASSWORD);
    t.send_text(CB_CHAT, "s3cret", 5).await.unwrap();
    assert_eq!(one(&mut rx).text, "📥 Вы получили 🪙 1 USDT ($1).");
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(3));
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(2));
}

#[tokio::test]
async fn xrocket_fresh_check_is_not_found_once_then_succeeds() {
    let Env { world, ub, t, .. } = env();
    world.set_xrocket_fresh_not_found(Some(Duration::from_millis(3000)));
    let check = world
        .create_check(CheckSpec::new(XR, usdt(dec!(10))))
        .unwrap();
    assert!(check.code.starts_with("t_") && check.code.len() == 17);
    assert_eq!(
        check.url,
        format!("https://t.me/xrocket?start={}", check.code)
    );
    let mut rx = t.subscribe();
    start(&t, XR_CHAT, &check.code, 1).await;
    assert_eq!(one(&mut rx).text, "Этот перевод не найден.");
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(0));
    start(&t, XR_CHAT, &check.code, 2).await;
    let versions = drain(&mut rx);
    assert_eq!(
        versions[0].text,
        format!("⏱ Получение 10{NBSP}USDT (10.0$)")
    );
    assert_eq!(
        versions.last().unwrap().text,
        format!("💰 Вы успешно получили 10{NBSP}USDT (10.0$).")
    );
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(10));
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
    assert_eq!(
        one(&mut rx).text,
        format!("💰 Вы успешно получили 0.5{NBSP}TONCOIN (2.5$).")
    );
    start(&t, XR_CHAT, &check.code, 2).await;
    assert_eq!(one(&mut rx).text, "Этот перевод уже активирован вами.");
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
    assert_eq!(one(&mut rx).text, "📥 Вы получили 🪙 7 USDT ($7).");
    let before = world.messages(ub, CB_CHAT);
    let err = t
        .send_text(CB_CHAT, &format!("/start {}", start_param(&url)), 42)
        .await
        .unwrap_err();
    assert_eq!(err, TransportError::RandomIdDuplicate);
    assert!(drain(&mut rx).is_empty());
    assert_eq!(world.messages(ub, CB_CHAT), before);
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(7));
    // random_id общий для всех чатов аккаунта.
    assert_eq!(
        t.send_text(Chat::SavedMessages, "note", 42)
            .await
            .unwrap_err(),
        TransportError::RandomIdDuplicate
    );
}

// ---------- меню чеков CryptoBot ----------

/// `/checks` → «Создать чек» → USDT: экран ввода суммы (сообщение меню).
async fn cb_amount_prompt(
    t: &SimTransport,
    rx: &mut broadcast::Receiver<RawMessage>,
) -> RawMessage {
    t.send_text(CB_CHAT, "/checks", 900).await.unwrap();
    let menu = one(rx);
    assert_eq!(menu.text, texts::cb_checks_menu());
    press(t, CB_CHAT, &menu, "Создать чек").await;
    let coins = one(rx);
    assert_eq!(coins.id, menu.id, "бот правит меню");
    assert_eq!(coins.text, texts::CB_CHOOSE_COIN);
    press(t, CB_CHAT, &coins, "USDT").await;
    let prompt = one(rx);
    assert_eq!(prompt.id, menu.id);
    assert!(prompt.text.starts_with("Пришлите сумму чека в USDT."));
    prompt
}

#[tokio::test]
async fn cryptobot_menu_asks_for_activations_and_skip_creates_one_check() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, CB, Asset::Usdt, dec!(100)).unwrap();
    let mut rx = t.subscribe();
    let prompt = cb_amount_prompt(&t, &mut rx).await;
    assert!(prompt.text.ends_with("Ваш баланс: 100 USDT ($100)"));

    // Сумма укладывается в баланс 10 раз: новое сообщение «Пришлите количество активаций…».
    let amount = t.send_text(CB_CHAT, "10", 77).await.unwrap();
    let ask = one(&mut rx);
    assert!(ask.id > amount.id, "ответ на сумму — новое сообщение");
    assert_eq!(ask.text, texts::cb_ask_activations(10));
    assert_eq!(
        classify_check_creation(CB, &ask.text, &urls(&ask)),
        CheckCreationReply::AskActivations
    );
    assert!(world.checks().is_empty(), "чек ещё не создан");
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(100));

    // «Пропустить ›» — одна активация; экран чека — правкой того же сообщения.
    press(&t, CB_CHAT, &ask, "Пропустить").await;
    let screen = one(&mut rx);
    assert_eq!(screen.id, ask.id);
    let created = match classify_check_creation(CB, &screen.text, &urls(&screen)) {
        CheckCreationReply::Created(c) => c,
        other => panic!("{other:?}: {screen:#?}"),
    };
    assert_eq!(created.amount, Some((dec!(10), Asset::Usdt)));
    let checks = world.checks_created_by(ub);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].code, created.param);
    assert_eq!(checks[0].activations, 1);
    assert_eq!(checks[0].origin, CheckOrigin::Menu { random_id: 77 });
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(90));
    // Кнопки «Пропустить» на экране чека уже нет: второго чека нажатием не сделать.
    assert!(screen.find_button("Пропустить").is_none());
    assert!(
        t.press(CB_CHAT, ask.id, &data(&ask, "Пропустить"))
            .await
            .is_err()
    );
    assert_eq!(world.checks().len(), 1);
}

#[tokio::test]
async fn cryptobot_menu_creates_the_check_on_the_amount_when_no_multicheck_fits() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, CB, Asset::Usdt, dec!(5.05)).unwrap();
    let mut rx = t.subscribe();
    cb_amount_prompt(&t, &mut rx).await;
    let amount = t.send_text(CB_CHAT, "4", 5).await.unwrap();
    let screen = one(&mut rx);
    assert!(screen.id > amount.id);
    assert_eq!(
        screen.text,
        texts::cb_check(&usdt(dec!(4)), dec!(4), &world.checks()[0].code)
    );
    assert!(matches!(
        classify_check_creation(CB, &screen.text, &urls(&screen)),
        CheckCreationReply::Created(_)
    ));
    assert_eq!(world.checks()[0].origin, CheckOrigin::Menu { random_id: 5 });
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(1.05));
}

#[tokio::test]
async fn cryptobot_menu_reports_insufficient_funds_and_bad_amount() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, CB, Asset::Usdt, dec!(5)).unwrap();
    let mut rx = t.subscribe();
    cb_amount_prompt(&t, &mut rx).await;
    t.send_text(CB_CHAT, "abc", 1).await.unwrap();
    assert!(one(&mut rx).text.starts_with("Неверная сумма"));
    t.send_text(CB_CHAT, "10", 2).await.unwrap();
    let reply = one(&mut rx);
    assert_eq!(
        reply.text,
        "Недостаточно монет. Пополните баланс.\n\nВаш баланс: 5 USDT ($5)."
    );
    assert_eq!(
        classify_check_creation(CB, &reply.text, &urls(&reply)),
        CheckCreationReply::InsufficientFunds
    );
    assert!(world.checks().is_empty());
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(5));
}

#[tokio::test]
async fn cryptobot_check_is_deleted_with_confirmation() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, CB, Asset::Usdt, dec!(5)).unwrap();
    let mut rx = t.subscribe();
    cb_amount_prompt(&t, &mut rx).await;
    t.send_text(CB_CHAT, "4", 1).await.unwrap();
    let screen = one(&mut rx);
    let code = world.checks()[0].code.clone();
    press(&t, CB_CHAT, &screen, "Удалить чек").await;
    let confirm = one(&mut rx);
    assert_eq!(confirm.text, texts::CB_CONFIRM_DELETE);
    press(&t, CB_CHAT, &confirm, "Да").await;
    let versions = drain(&mut rx);
    assert_eq!(versions[0].text, texts::CB_DELETING);
    assert_eq!(versions.last().unwrap().text, texts::cb_checks_menu());
    assert_eq!(world.check(&code).unwrap().status, CheckStatus::Cancelled);
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(5));
}

// ---------- меню чеков xRocket ----------

/// `/cheques` → «Персональный» → «Создать чек» → USDT: экран ввода суммы (сообщение меню).
async fn xr_amount_prompt(
    t: &SimTransport,
    rx: &mut broadcast::Receiver<RawMessage>,
) -> RawMessage {
    t.send_text(XR_CHAT, "/cheques", 900).await.unwrap();
    let menu = one(rx);
    assert_eq!(menu.text, texts::XR_CHEQUES_MENU);
    press(t, XR_CHAT, &menu, "Персональный").await;
    let personal = one(rx);
    assert_eq!(personal.text, texts::XR_PERSONAL);
    press(t, XR_CHAT, &personal, "Создать чек").await;
    let coins = one(rx);
    assert_eq!(coins.text, texts::XR_CHOOSE_COIN);
    press(t, XR_CHAT, &coins, "USDT").await;
    let prompt = one(rx);
    assert_eq!(prompt.id, menu.id, "всё — правки одного сообщения");
    assert!(prompt.text.ends_with("Введите сумму чека в USDT:"));
    prompt
}

#[tokio::test]
async fn xrocket_menu_confirms_the_amount_and_creates_the_check_on_confirm() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, XR, Asset::Usdt, dec!(50)).unwrap();
    let mut rx = t.subscribe();
    let prompt = xr_amount_prompt(&t, &mut rx).await;

    // Ответ на сумму — правка сообщения меню, оно старше нашей суммы.
    let amount = t.send_text(XR_CHAT, "10", 31).await.unwrap();
    let confirm = one(&mut rx);
    assert_eq!(confirm.id, prompt.id);
    assert!(confirm.id < amount.id);
    assert_eq!(
        confirm.text,
        format!(
            "🏷 Персональный чек\n\nСумма чека: 10{NBSP}USDT (10.0$)\n\nПожалуйста, подтвердите корректность данных:"
        )
    );
    assert_eq!(
        classify_check_creation(XR, &confirm.text, &urls(&confirm)),
        CheckCreationReply::Confirm {
            amount: Some((dec!(10), Asset::Usdt))
        }
    );
    assert!(world.checks().is_empty(), "сумма чек не создаёт");

    press(&t, XR_CHAT, &confirm, "Подтверждаю").await;
    let screen = one(&mut rx);
    assert_eq!(screen.id, prompt.id);
    let created = match classify_check_creation(XR, &screen.text, &urls(&screen)) {
        CheckCreationReply::Created(c) => c,
        other => panic!("{other:?}: {screen:#?}"),
    };
    assert_eq!(created.amount, Some((dec!(10), Asset::Usdt)));
    let checks = world.checks_created_by(ub);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].code, created.param);
    assert!(checks[0].code.starts_with("t_"));
    assert_eq!(checks[0].origin, CheckOrigin::Menu { random_id: 31 });
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(40));
    // Повторное «Подтверждаю» невозможно: на экране чека его нет.
    assert!(
        t.press(XR_CHAT, confirm.id, &data(&confirm, "Подтверждаю"))
            .await
            .is_err()
    );
    assert_eq!(world.checks().len(), 1);
}

#[tokio::test]
async fn xrocket_menu_says_insufficient_on_the_same_screen_and_waits_for_another_amount() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, XR, Asset::Usdt, dec!(5.02)).unwrap();
    let mut rx = t.subscribe();
    let prompt = xr_amount_prompt(&t, &mut rx).await;
    t.send_text(XR_CHAT, "10", 1).await.unwrap();
    let screen = one(&mut rx);
    assert_eq!(screen.id, prompt.id);
    assert!(screen.text.ends_with("🔺 Недостаточно средств на балансе."));
    assert_eq!(
        classify_check_creation(XR, &screen.text, &urls(&screen)),
        CheckCreationReply::InsufficientFunds
    );
    assert!(world.checks().is_empty());
    t.send_text(XR_CHAT, "1", 2).await.unwrap();
    let confirm = one(&mut rx);
    assert!(matches!(
        classify_check_creation(XR, &confirm.text, &urls(&confirm)),
        CheckCreationReply::Confirm { .. }
    ));
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(5.02));
}

#[tokio::test]
async fn xrocket_delete_is_silent() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, XR, Asset::Usdt, dec!(5)).unwrap();
    let mut rx = t.subscribe();
    xr_amount_prompt(&t, &mut rx).await;
    t.send_text(XR_CHAT, "1", 1).await.unwrap();
    let confirm = one(&mut rx);
    press(&t, XR_CHAT, &confirm, "Подтверждаю").await;
    let screen = one(&mut rx);
    let code = world.checks()[0].code.clone();
    press(&t, XR_CHAT, &screen, "Удалить").await;
    let back = one(&mut rx);
    assert_eq!(
        back.id, screen.id,
        "никакого сообщения — только правка экрана"
    );
    assert_eq!(back.text, texts::XR_PERSONAL);
    assert_eq!(world.check(&code).unwrap().status, CheckStatus::Cancelled);
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(5));
}

#[tokio::test]
async fn money_press_faults_hit_only_the_pressing_that_creates_the_check() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, XR, Asset::Usdt, dec!(5)).unwrap();
    let mut rx = t.subscribe();
    // Сбой денежного нажатия не трогает обычные кнопки меню.
    world.inject(ub, Call::CheckPress, Fault::TimeoutAfterEffect, 1);
    xr_amount_prompt(&t, &mut rx).await;
    t.send_text(XR_CHAT, "1", 1).await.unwrap();
    let confirm = one(&mut rx);
    let err = t
        .press(XR_CHAT, confirm.id, &data(&confirm, "Подтверждаю"))
        .await
        .unwrap_err();
    assert_eq!(err, TransportError::Timeout);
    // Чек создан, экран пришёл в поток.
    let screen = one(&mut rx);
    assert!(matches!(
        classify_check_creation(XR, &screen.text, &urls(&screen)),
        CheckCreationReply::Created(_)
    ));
    assert_eq!(world.checks().len(), 1);
}

// ---------- сбои ----------

#[tokio::test]
async fn failures_before_effect_do_nothing_and_keep_random_id_free() {
    let Env { world, ub, t, .. } = env();
    let url = world.client_check(CB, usdt(dec!(3))).unwrap();
    let text = format!("/start {}", start_param(&url));
    for fault in [
        Fault::TimeoutBeforeEffect,
        Fault::Disconnected,
        Fault::FloodWait(Duration::from_secs(29)),
        Fault::Rpc {
            code: 400,
            name: "PEER_ID_INVALID".into(),
        },
    ] {
        world.inject(ub, Call::SendText, fault.clone(), 1);
        let err = t.send_text(CB_CHAT, &text, 5).await.unwrap_err();
        match fault {
            Fault::TimeoutBeforeEffect => assert_eq!(err, TransportError::Timeout),
            Fault::Disconnected => assert_eq!(err, TransportError::Disconnected),
            Fault::FloodWait(d) => assert_eq!(err, TransportError::FloodWait(d)),
            _ => assert!(matches!(err, TransportError::Rpc { code: 400, .. })),
        }
        assert!(world.messages(ub, CB_CHAT).is_empty(), "{fault:?}");
        assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(0));
    }
    // random_id не занят: та же отправка проходит один раз.
    t.send_text(CB_CHAT, &text, 5).await.unwrap();
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(3));
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
    assert_eq!(one(&mut rx).text, "📥 Вы получили 🪙 4 USDT ($4).");
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(4));
}

#[tokio::test]
async fn faults_apply_to_next_n_matching_calls_only() {
    let Env { world, ub, t, .. } = env();
    world.inject(ub, Call::Any, Fault::Disconnected, 2);
    world.inject(ub, Call::History, Fault::TimeoutBeforeEffect, 1);
    assert_eq!(
        t.send_text(CB_CHAT, "/wallet", 1).await.unwrap_err(),
        TransportError::Disconnected
    );
    assert_eq!(
        t.press(CB_CHAT, 1, b"x").await.unwrap_err(),
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
    assert!(reply.id > sent.id);
    tokio::task::yield_now().await;
    let last = settled(&mut rx).pop().unwrap_or(reply);
    assert_eq!(last.text, "📥 Вы получили 🪙 2 USDT ($2).");
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
    assert_eq!(first.text, texts::processing(XR));
    assert_eq!(first.edit_date, None);
    let fin = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fin.id, first.id, "правка того же сообщения");
    assert!(fin.edit_date.is_some());
    let done = format!("💰 Вы успешно получили 3{NBSP}USDT (3.0$).");
    assert_eq!(fin.text, done);
    let hist = t.history(XR_CHAT, 0, 10).await.unwrap();
    assert_eq!(hist.last().unwrap().text, done);
}

#[tokio::test]
async fn unknown_text_no_reply_and_edit_instead_of_new() {
    let Env { world, ub, t, .. } = env();
    let mut rx = t.subscribe();

    let url = world.client_check(CB, usdt(dec!(1))).unwrap();
    world.inject(
        ub,
        Call::SendText,
        Fault::UnknownReply("Что-то пошло не так.".into()),
        1,
    );
    start(&t, CB_CHAT, start_param(&url), 1).await;
    assert_eq!(one(&mut rx).text, "Что-то пошло не так.");
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
    assert_eq!(edited.text, "📥 Вы получили 🪙 4 USDT ($4).");
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(7));
}

// ---------- счета CryptoBot ----------

#[tokio::test]
async fn cryptobot_usdt_invoice_opens_on_confirmation_and_asks_to_trust_a_new_recipient() {
    let Env {
        world,
        ub,
        t,
        client,
    } = env();
    world.set_balance(ub, CB, Asset::Usdt, dec!(50)).unwrap();
    let inv = world
        .create_invoice(InvoiceSpec::crypto(CB, usdt(dec!(10))).by(client))
        .unwrap();
    let mut rx = t.subscribe();
    start(&t, CB_CHAT, &inv.code, 1).await;
    let screen = one(&mut rx);
    let number = match classify_invoice_screen(CB, &screen.text) {
        InvoiceScreen::Confirm { number, amount } => {
            assert_eq!(amount, Some((dec!(10), Asset::Usdt)));
            number.unwrap()
        }
        other => panic!("{other:?}: {screen:#?}"),
    };
    assert!(number.starts_with("IV") && number != inv.code);
    assert!(screen.find_button("Оплатить анонимно").is_some());

    // Новый получатель: «Оплатить» спрашивает о доверии, денег не списывает.
    press(&t, CB_CHAT, &screen, "💳 Оплатить").await;
    let trust = one(&mut rx);
    assert_eq!(trust.id, screen.id);
    assert_eq!(
        classify_invoice_screen(CB, &trust.text),
        InvoiceScreen::TrustRecipient
    );
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(50));

    press(&t, CB_CHAT, &trust, "Отправить платеж").await;
    let msgs = settled(&mut rx);
    assert_eq!(msgs.len(), 2, "{msgs:#?}");
    assert_eq!(msgs[0].text, texts::CB_PAID_MARK);
    assert_eq!(
        msgs[1].text,
        format!("📥 Вы оплатили счёт #{number} на сумму 🪙 10 USDT ($10).\n\nПолучатель: @client")
    );
    assert_eq!(
        classify_invoice_screen(CB, &msgs[1].text),
        InvoiceScreen::Paid {
            number: Some(number),
            amount: Some((dec!(10), Asset::Usdt))
        }
    );
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(40));
    assert_eq!(world.balance(client, CB, Asset::Usdt), dec!(10));
    assert_eq!(
        world.invoice(&inv.code).unwrap().status,
        SimInvoiceStatus::Paid
    );

    // Знакомому получателю — без вопроса: «Оплатить» сразу платит.
    let inv2 = world
        .create_invoice(InvoiceSpec::crypto(CB, usdt(dec!(5))).by(client))
        .unwrap();
    start(&t, CB_CHAT, &inv2.code, 2).await;
    let screen = one(&mut rx);
    press(&t, CB_CHAT, &screen, "💳 Оплатить").await;
    let msgs = settled(&mut rx);
    assert!(matches!(
        classify_invoice_screen(CB, &msgs.last().unwrap().text),
        InvoiceScreen::Paid { .. }
    ));
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(35));
    // Оплаченный счёт.
    start(&t, CB_CHAT, &inv.code, 3).await;
    assert_eq!(
        classify_invoice_screen(CB, &one(&mut rx).text),
        InvoiceScreen::AlreadyPaid
    );
}

#[tokio::test]
async fn cryptobot_fiat_invoice_shows_coins_with_amounts_on_buttons() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, CB, Asset::Usdt, dec!(50)).unwrap();
    world.set_fiat_rate(Asset::Usdt, "USD", dec!(0.998));
    let inv = world
        .create_invoice(InvoiceSpec::fiat(
            CB,
            dec!(5),
            "USD",
            &[Asset::Usdt, Asset::Ton],
        ))
        .unwrap();
    let mut rx = t.subscribe();
    start(&t, CB_CHAT, &inv.code, 1).await;
    let coins = one(&mut rx);
    assert!(
        coins
            .text
            .starts_with("Выберите монету для оплаты счета #IV")
    );
    assert!(coins.text.ends_with("на сумму $5."));
    assert!(matches!(
        classify_invoice_screen(CB, &coins.text),
        InvoiceScreen::ChooseCoin { .. }
    ));
    // 5 / 0.998 = 5.01002…, вверх до шага 0.01.
    let usdt_button = coins.find_button("USDT").unwrap().text.clone();
    assert_eq!(usdt_button, "USDT · 5.02 USDT");
    press(&t, CB_CHAT, &coins, "USDT").await;
    let confirm = one(&mut rx);
    assert!(matches!(
        classify_invoice_screen(CB, &confirm.text),
        InvoiceScreen::Confirm {
            amount: Some((a, Asset::Usdt)),
            ..
        } if a == dec!(5.02)
    ));
    press(&t, CB_CHAT, &confirm, "💳 Оплатить").await;
    let trust = one(&mut rx);
    press(&t, CB_CHAT, &trust, "Отправить платеж").await;
    let paid = settled(&mut rx).pop().unwrap();
    assert!(
        paid.text.contains("на сумму 5 USD (5.02 USDT)"),
        "{}",
        paid.text
    );
    assert!(matches!(
        classify_invoice_screen(CB, &paid.text),
        InvoiceScreen::Paid { amount: Some((a, Asset::Usdt)), .. } if a == dec!(5.02)
    ));
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(44.98));
}

#[tokio::test]
async fn cryptobot_invoice_without_funds_offers_networks_and_moves_nothing() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, CB, Asset::Usdt, dec!(3)).unwrap();
    let inv = world
        .create_invoice(InvoiceSpec::crypto(CB, usdt(dec!(10))))
        .unwrap();
    let mut rx = t.subscribe();
    start(&t, CB_CHAT, &inv.code, 1).await;
    let screen = one(&mut rx);
    press(&t, CB_CHAT, &screen, "💳 Оплатить").await;
    let net = one(&mut rx);
    assert_eq!(net.text, "Выберите сеть для оплаты счёта в USDT.");
    assert_eq!(
        classify_invoice_screen(CB, &net.text),
        InvoiceScreen::InsufficientFunds
    );
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(3));
    assert!(world.invoice(&inv.code).unwrap().payments.is_empty());
}

#[tokio::test]
async fn cryptobot_asks_for_the_passcode_over_the_24h_limit_and_moves_no_money() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, CB, Asset::Usdt, dec!(1000)).unwrap();
    world.set_passcode_free_limit(ub, Some(dec!(15))).unwrap();
    let mut rx = t.subscribe();
    let pay = |code: String, rid: i64| {
        let t = t.clone();
        async move {
            start(&t, CB_CHAT, &code, rid).await;
        }
    };
    let first = world
        .create_invoice(InvoiceSpec::crypto(CB, usdt(dec!(10))))
        .unwrap();
    pay(first.code.clone(), 1).await;
    let screen = one(&mut rx);
    press(&t, CB_CHAT, &screen, "💳 Оплатить").await;
    let trust = one(&mut rx);
    press(&t, CB_CHAT, &trust, "Отправить платеж").await;
    drain(&mut rx);
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(990));

    // 10 + 10 > 15 за сутки: бот просит код-пароль, деньги не двигаются.
    let second = world
        .create_invoice(InvoiceSpec::crypto(CB, usdt(dec!(10))))
        .unwrap();
    pay(second.code.clone(), 2).await;
    let screen = one(&mut rx);
    press(&t, CB_CHAT, &screen, "💳 Оплатить").await;
    let asked = one(&mut rx);
    assert_eq!(asked.text, texts::CB_PASSCODE_REQUIRED);
    assert_eq!(
        classify_invoice_screen(CB, &asked.text),
        InvoiceScreen::PasscodeRequired
    );
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(990));
    // Через сутки лимит снова свободен.
    world.advance(Duration::from_secs(24 * 3600 + 1));
    let third = world
        .create_invoice(InvoiceSpec::crypto(CB, usdt(dec!(10))))
        .unwrap();
    pay(third.code.clone(), 3).await;
    let screen = one(&mut rx);
    press(&t, CB_CHAT, &screen, "💳 Оплатить").await;
    let msgs = settled(&mut rx);
    assert!(matches!(
        classify_invoice_screen(CB, &msgs.last().unwrap().text),
        InvoiceScreen::Paid { .. }
    ));
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(980));
}

// ---------- счета xRocket ----------

#[tokio::test]
async fn xrocket_invoice_is_paid_via_the_wallet_method() {
    let Env {
        world,
        ub,
        t,
        client,
    } = env();
    world.set_balance(ub, XR, Asset::Usdt, dec!(5.02)).unwrap();
    let inv = world
        .create_invoice(InvoiceSpec::crypto(XR, usdt(dec!(2))).by(client))
        .unwrap();
    let mut rx = t.subscribe();
    start(&t, XR_CHAT, &inv.code, 1).await;
    let card = one(&mut rx);
    let number = match classify_invoice_screen(XR, &card.text) {
        InvoiceScreen::ChooseMethod { number, amount } => {
            assert_eq!(amount, Some((dec!(2), Asset::Usdt)));
            number.unwrap()
        }
        other => panic!("{other:?}: {card:#?}"),
    };
    assert!(number.starts_with("INV"));
    press(&t, XR_CHAT, &card, "xRocket").await;
    let confirm = one(&mut rx);
    assert_eq!(confirm.id, card.id);
    assert!(
        confirm
            .text
            .ends_with(&format!("🔸 Подтвердите ваш платёж на 2{NBSP}USDT (2.0$)"))
    );
    assert!(matches!(
        classify_invoice_screen(XR, &confirm.text),
        InvoiceScreen::Confirm {
            amount: Some((a, Asset::Usdt)),
            ..
        } if a == dec!(2)
    ));
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(5.02));
    press(&t, XR_CHAT, &confirm, "Оплатить").await;
    let msgs = settled(&mut rx);
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].text, texts::XR_LOADING);
    assert_eq!(
        classify_invoice_screen(XR, &msgs[0].text),
        InvoiceScreen::Progress
    );
    assert!(msgs[1].text.starts_with(&format!(
        "📋 Вы успешно оплатили счёт на сумму 2{NBSP}USDT (2.0$). ID платежа: #i"
    )));
    assert!(matches!(
        classify_invoice_screen(XR, &msgs[1].text),
        InvoiceScreen::Paid {
            amount: Some((a, Asset::Usdt)),
            ..
        } if a == dec!(2)
    ));
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(3.02));
    assert_eq!(world.balance(client, XR, Asset::Usdt), dec!(2));
    start(&t, XR_CHAT, &inv.code, 2).await;
    let again = one(&mut rx);
    assert_eq!(again.text, "Данный счёт уже оплачен.");
    assert_eq!(
        classify_invoice_screen(XR, &again.text),
        InvoiceScreen::AlreadyPaid
    );
}

#[tokio::test]
async fn xrocket_invoice_without_funds_says_so_and_moves_nothing() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, XR, Asset::Usdt, dec!(5.02)).unwrap();
    let inv = world
        .create_invoice(InvoiceSpec::crypto(XR, usdt(dec!(10))))
        .unwrap();
    let mut rx = t.subscribe();
    start(&t, XR_CHAT, &inv.code, 1).await;
    let card = one(&mut rx);
    press(&t, XR_CHAT, &card, "xRocket").await;
    let screen = one(&mut rx);
    assert!(screen.text.ends_with("🔺 Недостаточно средств на балансе."));
    assert_eq!(
        classify_invoice_screen(XR, &screen.text),
        InvoiceScreen::InsufficientFunds
    );
    assert!(screen.find_button("Оплатить").is_none());
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(5.02));
}

#[tokio::test]
async fn invoice_statuses_and_pay_press_timeout() {
    let Env {
        world,
        ub,
        t,
        client,
    } = env();
    world.set_balance(ub, XR, Asset::Usdt, dec!(10)).unwrap();
    let mut rx = t.subscribe();
    start(&t, XR_CHAT, "inv_AAAAAAAAAAAAAAA", 1).await;
    assert_eq!(
        classify_invoice_screen(XR, &one(&mut rx).text),
        InvoiceScreen::NotFound
    );
    let expired = world
        .create_invoice(InvoiceSpec::crypto(XR, usdt(dec!(1))))
        .unwrap();
    world.expire_invoice(&expired.code).unwrap();
    start(&t, XR_CHAT, &expired.code, 2).await;
    assert_eq!(
        classify_invoice_screen(XR, &one(&mut rx).text),
        InvoiceScreen::Expired
    );
    let own = world
        .create_invoice(InvoiceSpec::crypto(XR, usdt(dec!(1))).by(ub))
        .unwrap();
    start(&t, XR_CHAT, &own.code, 3).await;
    assert_eq!(one(&mut rx).text, texts::INVOICE_OWN);

    // «Оплатить» выполнено, ответ вызова потерян: квитанция всё равно в потоке, платёж один.
    let inv = world
        .create_invoice(InvoiceSpec::crypto(XR, usdt(dec!(3))).by(client))
        .unwrap();
    start(&t, XR_CHAT, &inv.code, 4).await;
    let card = one(&mut rx);
    press(&t, XR_CHAT, &card, "xRocket").await;
    let confirm = one(&mut rx);
    world.inject(ub, Call::PayPress, Fault::TimeoutAfterEffect, 1);
    assert_eq!(
        t.press(XR_CHAT, confirm.id, &data(&confirm, "Оплатить"))
            .await
            .unwrap_err(),
        TransportError::Timeout
    );
    let msgs = settled(&mut rx);
    assert!(matches!(
        classify_invoice_screen(XR, &msgs.last().unwrap().text),
        InvoiceScreen::Paid { .. }
    ));
    assert_eq!(world.invoice(&inv.code).unwrap().payments.len(), 1);
    assert_eq!(world.balance(ub, XR, Asset::Usdt), dec!(7));
}

// ---------- кошелёк, переводы, история ----------

#[tokio::test]
async fn wallet_screen_lists_balances_as_the_parser_reads_them() {
    let Env { world, ub, t, .. } = env();
    world.set_balance(ub, CB, Asset::Usdt, dec!(12.50)).unwrap();
    world.set_balance(ub, XR, Asset::Usdt, dec!(5.02)).unwrap();
    let mut rx = t.subscribe();
    t.send_text(CB_CHAT, "/wallet", 1).await.unwrap();
    let cb = one(&mut rx);
    assert!(
        cb.text
            .starts_with("👛 Кошелёк\n\n🪙 Tether: 12.5 USDT ($12.5)")
    );
    let usdt_line = |p: Platform, text: &str| {
        parse_balance(p, text)
            .into_iter()
            .find(|b| b.asset == Asset::Usdt)
            .map(|b| b.amount)
    };
    assert_eq!(usdt_line(CB, &cb.text), Some(dec!(12.5)));
    t.send_text(XR_CHAT, "/wallet", 2).await.unwrap();
    let xr = one(&mut rx);
    assert!(xr.text.starts_with(&format!(
        "👛 Мой кошелёк\n\n🪙{NBSP}USDT: 5.02{NBSP}USDT (5.0$)"
    )));
    assert_eq!(usdt_line(XR, &xr.text), Some(dec!(5.02)));
    assert_eq!(world.balances(ub, XR), vec![usdt(dec!(5.02)), ton(dec!(0))]);
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
    assert_eq!(msg.text, "📥 Вы получили 🪙 5 USDT ($5) от @x.");
    assert_eq!(
        classify_activation(CB, &msg.text),
        ActivationReply::IncomingTransfer {
            amount: dec!(5),
            asset: Asset::Usdt
        }
    );
    assert_eq!(world.balance(ub, CB, Asset::Usdt), dec!(5));
    let ad = world.bot_message(ub, XR, "🚀 Реклама").unwrap();
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
    press(&t, CB_CHAT, &menu, "Создать чек").await;

    let all = t.history(CB_CHAT, 0, 100).await.unwrap();
    assert_eq!(all.len(), 4);
    assert!(all.windows(2).all(|w| w[0].id < w[1].id));
    assert_eq!(all[0], first);
    assert_eq!(
        all[1].text,
        texts::CB_CHOOSE_COIN,
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
async fn press_rejects_unknown_message_or_data_and_answers_unsupported_buttons() {
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
        name(t.press(CB_CHAT, menu.id, b"count-1").await.unwrap_err()),
        "DATA_INVALID",
        "кнопки нет на экране"
    );
    assert_eq!(
        name(t.press(CB_CHAT, sent.id, b"create").await.unwrap_err()),
        "DATA_INVALID",
        "своё сообщение"
    );
    assert_eq!(
        name(t.press(XR_CHAT, menu.id, b"create").await.unwrap_err()),
        "MESSAGE_ID_INVALID"
    );
    // Кнопка, которую exch не нажимает: всплывающее окно, экран не меняется.
    let answer = t
        .press(CB_CHAT, menu.id, &data(&menu, "‹ Назад"))
        .await
        .unwrap();
    assert!(answer.alert);
    assert!(drain(&mut rx).is_empty());
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
    // …и выплачивает чеком xRocket через меню с другого аккаунта, Боб его забирает.
    let mut rx = t_xr.subscribe();
    xr_amount_prompt(&t_xr, &mut rx).await;
    t_xr.send_text(XR_CHAT, "39", 2).await.unwrap();
    let confirm = one(&mut rx);
    press(&t_xr, XR_CHAT, &confirm, "Подтверждаю").await;
    let screen = one(&mut rx);
    let url = match classify_check_creation(XR, &screen.text, &urls(&screen)) {
        CheckCreationReply::Created(c) => c.url,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        world.claim_check(bob, &url).unwrap(),
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
    assert_eq!(one(&mut rx).text, texts::not_found(CB, &check.code));
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
