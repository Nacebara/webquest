//! Сценарии юзербота (`UserbotWallet`) против симулятора @send и @xrocket (DESIGN-v0.2 §4, §7):
//! каждый путь, каждый класс ответа, сбои на шагах. Денежные инварианты: на операцию не больше
//! одного созданного чека и одной оплаты; `Unknown` не превращается в повтор.
#![cfg(feature = "sim")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use domain::{Asset, Decimal, Money, Platform};
use rust_decimal_macros::dec;
use tokio::sync::broadcast;
use userbot::flows::{FlowConfig, MemoryLog, rid};
use userbot::sim::{
    AccountId, Call, CheckOrigin, CheckSpec, Fault, InvoiceSpec, SimInvoiceStatus, SimTransport,
    SimWorld,
};
use userbot::transport::{CallbackAnswer, Chat, RawMessage, Transport, TransportError};
use userbot::{
    AccountGate, ActivationOutcome, BotError, IssueMethod, IssueOutcome, OpTag, PayDue, PayOutcome,
    RejectReason, UserbotWallet, WalletBot,
};

const CB: Platform = Platform::CryptoBot;
const XR: Platform = Platform::XRocket;

fn usdt(x: Decimal) -> Money {
    Money::new(x, Asset::Usdt).unwrap()
}

fn ton(x: Decimal) -> Money {
    Money::new(x, Asset::Ton).unwrap()
}

fn tag(key: &str) -> OpTag {
    OpTag {
        key: key.to_owned(),
        random_id: rid::new_random_id(),
    }
}

fn param(url: &str) -> &str {
    url.split("start=").nth(1).unwrap()
}

/// Транспорт симулятора, который включает сбой ровно на нужной команде или кнопке.
struct Scripted {
    inner: SimTransport,
    world: SimWorld,
    account: AccountId,
    on_text: Mutex<Vec<(String, Fault)>>,
    on_recent: Mutex<Vec<(Chat, Fault)>>,
}

impl Scripted {
    fn fault_on_text(&self, text: &str, fault: Fault) {
        self.on_text.lock().unwrap().push((text.to_owned(), fault));
    }

    fn fault_on_recent(&self, chat: Chat, fault: Fault) {
        self.on_recent.lock().unwrap().push((chat, fault));
    }

    fn arm(&self, rules: &Mutex<Vec<(String, Fault)>>, hit: impl Fn(&str) -> bool, call: Call) {
        let mut rules = rules.lock().unwrap();
        if let Some(i) = rules.iter().position(|(k, _)| hit(k)) {
            let (_, fault) = rules.remove(i);
            self.world.inject(self.account, call, fault, 1);
        }
    }
}

#[async_trait]
impl Transport for Scripted {
    async fn send_text(
        &self,
        chat: Chat,
        text: &str,
        random_id: i64,
    ) -> Result<RawMessage, TransportError> {
        self.arm(&self.on_text, |k| k == text, Call::SendText);
        self.inner.send_text(chat, text, random_id).await
    }

    async fn press(
        &self,
        chat: Chat,
        msg_id: i32,
        data: &[u8],
    ) -> Result<CallbackAnswer, TransportError> {
        self.inner.press(chat, msg_id, data).await
    }

    async fn history(
        &self,
        chat: Chat,
        after_id: i32,
        limit: u32,
    ) -> Result<Vec<RawMessage>, TransportError> {
        self.inner.history(chat, after_id, limit).await
    }

    async fn recent(&self, chat: Chat, limit: u32) -> Result<Vec<RawMessage>, TransportError> {
        let fault = {
            let mut rules = self.on_recent.lock().unwrap();
            rules
                .iter()
                .position(|(c, _)| *c == chat)
                .map(|i| rules.remove(i).1)
        };
        if let Some(fault) = fault {
            self.world.inject(self.account, Call::History, fault, 1);
        }
        self.inner.recent(chat, limit).await
    }

    fn subscribe(&self) -> broadcast::Receiver<RawMessage> {
        self.inner.subscribe()
    }
}

struct Env {
    world: SimWorld,
    ub: AccountId,
    client: AccountId,
    shop: AccountId,
    transport: Arc<Scripted>,
    gate: Arc<AccountGate>,
    log: Arc<MemoryLog>,
}

fn env() -> Env {
    let world = SimWorld::new();
    let ub = world.add_account("exch_ub");
    let client = world.add_account("client");
    let shop = world.add_account("shop");
    let transport = Arc::new(Scripted {
        inner: world.transport(ub),
        world: world.clone(),
        account: ub,
        on_text: Mutex::new(Vec::new()),
        on_recent: Mutex::new(Vec::new()),
    });
    Env {
        world,
        ub,
        client,
        shop,
        transport,
        gate: Arc::new(AccountGate::new("ub-1")),
        log: Arc::new(MemoryLog::default()),
    }
}

impl Env {
    fn wallet(&self, platform: Platform) -> UserbotWallet<Scripted> {
        UserbotWallet::new(platform, self.transport.clone(), self.gate.clone())
            .with_log(self.log.clone())
    }

    fn balance(&self, platform: Platform) -> Decimal {
        self.world.balance(self.ub, platform, Asset::Usdt)
    }

    fn fund(&self, platform: Platform, amount: Decimal) {
        self.world
            .set_balance(self.ub, platform, Asset::Usdt, amount)
            .unwrap();
    }

    fn sent_texts(&self, chat: Chat) -> Vec<String> {
        self.world
            .messages(self.ub, chat)
            .into_iter()
            .filter(|m| m.out)
            .map(|m| m.text)
            .collect()
    }
}

// ============================== активация ==============================

#[tokio::test(start_paused = true)]
async fn activation_receives_money_and_logs_before_parsing() {
    let e = env();
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    let t = tag("ord-1-intake-1");
    let out = e.wallet(CB).activate_check(param(&url), &t).await.unwrap();
    assert_eq!(out, ActivationOutcome::Received(usdt(dec!(100))));
    assert_eq!(e.balance(CB), dec!(100));

    let entries = e.log.entries();
    let cmd = entries
        .iter()
        .find(|l| l.message.out && l.message.text.starts_with("/start"))
        .unwrap();
    assert_eq!(cmd.op.as_deref(), Some("ord-1-intake-1"));
    assert_eq!(cmd.account, "ub-1");
    let first = entries
        .iter()
        .position(|l| !l.message.out && l.op.is_none())
        .expect("ответ записан при получении");
    let attributed = entries
        .iter()
        .position(|l| !l.message.out && l.op.as_deref() == Some("ord-1-intake-1"))
        .expect("ответ признан ответом операции");
    assert!(first < attributed, "запись — до разбора");
}

#[tokio::test(start_paused = true)]
async fn every_rejection_maps_to_its_reason_and_moves_no_money() {
    let e = env();
    let five = usdt(dec!(5));
    let check = |spec: CheckSpec| e.world.create_check(spec).unwrap().url;

    let claimed = e.world.client_check(CB, five).unwrap();
    e.world.claim_check(e.client, &claimed).unwrap();
    let cases = [
        (claimed, RejectReason::AlreadyActivated),
        (
            "https://t.me/send?start=CQzzzzzzzzzz".to_owned(),
            RejectReason::NotFound,
        ),
        (
            check(CheckSpec::new(CB, five).personal_for(e.client)),
            RejectReason::NotForUs,
        ),
        (
            check(CheckSpec::new(CB, five).with_subscription("@chan")),
            RejectReason::NeedsSubscription,
        ),
        (
            check(CheckSpec::new(CB, five).with_captcha()),
            RejectReason::Captcha,
        ),
        (
            check(CheckSpec::new(CB, five).with_password("secret")),
            RejectReason::PasswordProtected,
        ),
        (
            check(CheckSpec::new(CB, five).premium_only()),
            RejectReason::PremiumOnly,
        ),
    ];
    let wallet = e.wallet(CB);
    for (i, (url, reason)) in cases.iter().enumerate() {
        let out = wallet
            .activate_check(param(url), &tag(&format!("ord-{i}-intake-1")))
            .await
            .unwrap();
        assert_eq!(out, ActivationOutcome::Rejected(*reason), "{url}");
    }
    assert_eq!(e.balance(CB), dec!(0));
}

#[tokio::test(start_paused = true)]
async fn xrocket_fresh_not_found_is_retried_once() {
    let e = env();
    e.world
        .set_xrocket_fresh_not_found(Some(Duration::from_secs(10)));
    let url = e.world.client_check(XR, usdt(dec!(50))).unwrap();
    let out = e
        .wallet(XR)
        .activate_check(param(&url), &tag("ord-1-intake-1"))
        .await
        .unwrap();
    assert_eq!(out, ActivationOutcome::Received(usdt(dec!(50))));
    assert_eq!(
        e.sent_texts(Chat::WalletBot(XR)).len(),
        2,
        "ровно один повтор"
    );

    // Без повтора — честный отказ «не найден», деньги остались в чеке.
    let url = e.world.client_check(XR, usdt(dec!(7))).unwrap();
    let no_retry = e.wallet(XR).with_config(FlowConfig {
        xrocket_not_found_retry: None,
        ..FlowConfig::default()
    });
    let out = no_retry
        .activate_check(param(&url), &tag("ord-2-intake-1"))
        .await
        .unwrap();
    assert_eq!(out, ActivationOutcome::Rejected(RejectReason::NotFound));
    assert_eq!(e.balance(XR), dec!(50));
}

#[tokio::test(start_paused = true)]
async fn processing_message_then_final_edit_is_received() {
    let e = env();
    let url = e.world.client_check(CB, usdt(dec!(20))).unwrap();
    e.world.inject(
        e.ub,
        Call::SendText,
        Fault::ProcessingFirst {
            final_after: Duration::from_secs(3),
        },
        1,
    );
    let out = e
        .wallet(CB)
        .activate_check(param(&url), &tag("ord-1-intake-1"))
        .await
        .unwrap();
    assert_eq!(out, ActivationOutcome::Received(usdt(dec!(20))));
}

#[tokio::test(start_paused = true)]
async fn late_reply_does_not_leak_into_the_next_operation() {
    let e = env();
    let wallet = e.wallet(CB);
    let first = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    e.world.inject(
        e.ub,
        Call::SendText,
        Fault::DelayReply(Duration::from_secs(25)),
        1,
    );
    let out = wallet
        .activate_check(param(&first), &tag("ord-1-intake-1"))
        .await
        .unwrap();
    assert!(
        matches!(out, ActivationOutcome::Unknown { raw: None }),
        "{out:?}"
    );
    assert_eq!(e.balance(CB), dec!(100), "деньги пришли — движок сверит");

    // «📥 Вы получили 🪙 100 USDT» придёт во время следующей операции и не будет её ответом,
    // даже если её собственный ответ задержится.
    let second = e.world.client_check(CB, usdt(dec!(7))).unwrap();
    e.world.inject(
        e.ub,
        Call::SendText,
        Fault::DelayReply(Duration::from_secs(10)),
        1,
    );
    let out = wallet
        .activate_check(param(&second), &tag("ord-2-intake-1"))
        .await
        .unwrap();
    assert_eq!(out, ActivationOutcome::Received(usdt(dec!(7))));
    let late = e
        .log
        .entries()
        .into_iter()
        .find(|l| l.message.text == "📥 Вы получили 🪙 100 USDT ($100).")
        .expect("поздний ответ записан");
    assert_eq!(late.op, None);
}

#[tokio::test(start_paused = true)]
async fn silence_unknown_text_and_old_message_edit_give_unknown() {
    let e = env();
    let wallet = e.wallet(CB);

    let url = e.world.client_check(CB, usdt(dec!(1))).unwrap();
    e.world.inject(e.ub, Call::SendText, Fault::NoReply, 1);
    let out = wallet
        .activate_check(param(&url), &tag("ord-1-intake-1"))
        .await
        .unwrap();
    assert_eq!(out, ActivationOutcome::Unknown { raw: None });

    // Чат занят: на прошлую команду нет ответа — новая команда не уходит, ничего не выполнено.
    let url = e.world.client_check(CB, usdt(dec!(2))).unwrap();
    let refused = wallet
        .activate_check(param(&url), &tag("ord-2-intake-1"))
        .await;
    assert!(
        matches!(refused, Err(BotError::Unavailable(_))),
        "{refused:?}"
    );
    assert_eq!(e.sent_texts(Chat::WalletBot(CB)).len(), 1);
    // Чат после активации без ответа сам не освобождается и через `unanswered_block`: её поздний
    // ответ несёт сумму (аудит 11.11.2026, Ю-2). Открывает его человек, разобрав заявку.
    tokio::time::advance(FlowConfig::default().unanswered_block).await;
    let still = wallet
        .activate_check(param(&url), &tag("ord-2-intake-1"))
        .await;
    assert!(matches!(still, Err(BotError::ChatBlocked)), "{still:?}");
    assert_eq!(e.sent_texts(Chat::WalletBot(CB)).len(), 1);
    wallet.acknowledge_unanswered().await;

    e.world.inject(
        e.ub,
        Call::SendText,
        Fault::UnknownReply("Технические работы, загляните позже".to_owned()),
        1,
    );
    let out = wallet
        .activate_check(param(&url), &tag("ord-2-intake-1"))
        .await
        .unwrap();
    match out {
        ActivationOutcome::Unknown { raw: Some(raw) } => {
            assert_eq!(raw.text, "Технические работы, загляните позже");
        }
        other => panic!("{other:?}"),
    }

    // Бот правит старое сообщение вместо ответа: это не ответ на нашу команду.
    let url = e.world.client_check(CB, usdt(dec!(4))).unwrap();
    e.world
        .inject(e.ub, Call::SendText, Fault::EditInsteadOfNew, 1);
    let out = wallet
        .activate_check(param(&url), &tag("ord-3-intake-1"))
        .await
        .unwrap();
    assert_eq!(out, ActivationOutcome::Unknown { raw: None });
    assert_eq!(e.balance(CB), dec!(7), "во всех трёх случаях деньги пришли");
}

#[tokio::test(start_paused = true)]
async fn timeout_after_send_resends_with_the_same_random_id() {
    let e = env();
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    e.world
        .inject(e.ub, Call::SendText, Fault::TimeoutAfterEffect, 1);
    let out = e
        .wallet(CB)
        .activate_check(param(&url), &tag("ord-1-intake-1"))
        .await
        .unwrap();
    assert_eq!(out, ActivationOutcome::Received(usdt(dec!(100))));
    assert_eq!(
        e.sent_texts(Chat::WalletBot(CB)).len(),
        1,
        "второго /start нет"
    );
}

#[tokio::test(start_paused = true)]
async fn repeated_activation_with_the_same_tag_is_idempotent() {
    let e = env();
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    let t = tag("ord-1-intake-1");
    let wallet = e.wallet(CB);
    let first = wallet.activate_check(param(&url), &t).await.unwrap();
    let again = wallet.activate_check(param(&url), &t).await.unwrap();
    assert_eq!(first, ActivationOutcome::Received(usdt(dec!(100))));
    assert_eq!(again, first, "тот же ответ, а не «уже активирован»");
    assert_eq!(e.sent_texts(Chat::WalletBot(CB)).len(), 1);
    assert_eq!(e.balance(CB), dec!(100));
}

/// Третья запись владельца: свой чек. CryptoBot — «🕘 Получение…», уведомление «… активировал(а)
/// ваш чек и получил(а) …» (тоже с суммой!), затем правка «📥 Вы получили …»; xRocket —
/// уведомление после правки. Ответ — правка, а не уведомление; деньги одни; позднее уведомление
/// не становится ответом следующей операции.
#[tokio::test(start_paused = true)]
async fn own_check_notice_is_never_the_answer() {
    for platform in [CB, XR] {
        let e = env();
        e.fund(platform, dec!(10));
        let own = e
            .world
            .create_check(CheckSpec::new(platform, usdt(dec!(3))).by(e.ub))
            .unwrap();
        let out = e
            .wallet(platform)
            .activate_check(&own.code, &tag("ord-1-intake-1"))
            .await
            .unwrap();
        assert_eq!(
            out,
            ActivationOutcome::Received(usdt(dec!(3))),
            "{platform:?}"
        );
        assert_eq!(e.balance(platform), dec!(10));
        if platform == CB {
            // Уведомление пришло посреди ответа: сценарий его видел, записал и пропустил.
            let notices: Vec<_> = e
                .log
                .entries()
                .into_iter()
                .filter(|l| l.message.text.contains("активировал(а) ваш чек"))
                .collect();
            assert_eq!(notices.len(), 1);
            assert_eq!(notices[0].op, None);
        }
        // Следующая операция получает свой ответ, а не хвост предыдущей.
        let next = e
            .world
            .create_check(CheckSpec::new(platform, usdt(dec!(2))))
            .unwrap();
        let out = e
            .wallet(platform)
            .activate_check(&next.code, &tag("ord-2-intake-1"))
            .await
            .unwrap();
        assert_eq!(
            out,
            ActivationOutcome::Received(usdt(dec!(2))),
            "{platform:?}"
        );
        assert_eq!(e.balance(platform), dec!(12));
        assert!(
            e.log
                .entries()
                .iter()
                .filter(|l| l.message.text.contains("активировал(а) ваш чек"))
                .all(|l| l.op.is_none()),
            "{platform:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn incoming_transfer_while_waiting_is_not_a_win() {
    let e = env();
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    e.world.inject(
        e.ub,
        Call::SendText,
        Fault::DelayReply(Duration::from_secs(5)),
        1,
    );
    let world = e.world.clone();
    let ub = e.ub;
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        world
            .incoming_transfer(ub, CB, usdt(dec!(5)), "@x")
            .unwrap();
    });
    let out = e
        .wallet(CB)
        .activate_check(param(&url), &tag("ord-1-intake-1"))
        .await
        .unwrap();
    assert_eq!(out, ActivationOutcome::Received(usdt(dec!(100))));
}

#[tokio::test(start_paused = true)]
async fn flood_wait_pauses_the_whole_account() {
    let e = env();
    let url = e.world.client_check(CB, usdt(dec!(10))).unwrap();
    e.world.inject(
        e.ub,
        Call::SendText,
        Fault::FloodWait(Duration::from_secs(29)),
        1,
    );
    let cb = e.wallet(CB);
    let xr = e.wallet(XR);
    let out = cb.activate_check(param(&url), &tag("ord-1-intake-1")).await;
    assert_eq!(out, Err(BotError::FloodWait(Duration::from_secs(29))));
    // Другая платформа на том же аккаунте тоже стоит.
    assert!(matches!(xr.balances().await, Err(BotError::FloodWait(_))));
    assert_eq!(e.balance(CB), dec!(0));

    tokio::time::advance(Duration::from_secs(30)).await;
    let out = cb
        .activate_check(param(&url), &tag("ord-1-intake-2"))
        .await
        .unwrap();
    assert_eq!(out, ActivationOutcome::Received(usdt(dec!(10))));
}

#[tokio::test(start_paused = true)]
async fn two_platforms_on_one_account_take_turns() {
    let e = env();
    let cb_url = e.world.client_check(CB, usdt(dec!(10))).unwrap();
    let xr_url = e.world.client_check(XR, usdt(dec!(20))).unwrap();
    let (cb, xr) = (e.wallet(CB), e.wallet(XR));
    let (t1, t2) = (tag("ord-1-intake-1"), tag("ord-2-intake-1"));
    let (a, b) = tokio::join!(
        cb.activate_check(param(&cb_url), &t1),
        xr.activate_check(param(&xr_url), &t2),
    );
    assert_eq!(a.unwrap(), ActivationOutcome::Received(usdt(dec!(10))));
    assert_eq!(b.unwrap(), ActivationOutcome::Received(usdt(dec!(20))));
}

// ============================== выдача чека ==============================

async fn issue_once(platform: Platform, fund: Decimal, amount: Decimal) {
    let e = env();
    e.fund(platform, fund);
    let total = e.world.total_in_world(platform, Asset::Usdt);
    let t = tag("ord-7-payout");
    let out = e
        .wallet(platform)
        .issue_check(&usdt(amount), &t)
        .await
        .unwrap();
    let IssueOutcome::Issued { check, method } = out else {
        panic!("{platform:?}: {out:?}");
    };
    assert_eq!(method, IssueMethod::Menu);
    assert_eq!(check.amount, usdt(amount));

    let created = e.world.checks_created_by(e.ub);
    assert_eq!(created.len(), 1, "{platform:?}");
    assert_eq!(created[0].code, check.param);
    assert_eq!(created[0].url, check.url);
    assert_eq!(created[0].activations, 1, "не мультичек");
    assert!(matches!(
        created[0].origin,
        CheckOrigin::Menu { random_id } if random_id != 0
    ));
    assert_eq!(e.balance(platform), fund - amount);
    assert_eq!(e.world.total_in_world(platform, Asset::Usdt), total);

    let saved = e.world.messages(e.ub, Chat::SavedMessages);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].text, userbot::flows::marker_text("ord-7-payout"));
    let command = match platform {
        CB => "/checks",
        XR => "/cheques",
    };
    let amount_text = amount.normalize().to_string();
    assert_eq!(
        e.sent_texts(Chat::WalletBot(platform)),
        vec![command.to_owned(), amount_text]
    );

    // Клиент забирает чек — деньги у него.
    e.world.claim_check(e.client, &check.url).unwrap();
    assert_eq!(e.world.balance(e.client, platform, Asset::Usdt), amount);
}

#[tokio::test(start_paused = true)]
async fn cryptobot_check_via_skip_when_a_multicheck_would_fit() {
    issue_once(CB, dec!(500), dec!(97.5)).await;
}

#[tokio::test(start_paused = true)]
async fn cryptobot_check_on_the_amount_when_no_multicheck_fits() {
    issue_once(CB, dec!(150), dec!(97.5)).await;
}

#[tokio::test(start_paused = true)]
async fn xrocket_check_via_confirmation() {
    issue_once(XR, dec!(500), dec!(97.5)).await;
}

#[tokio::test(start_paused = true)]
async fn insufficient_funds_creates_nothing() {
    for platform in [CB, XR] {
        let e = env();
        e.fund(platform, dec!(10));
        let out = e
            .wallet(platform)
            .issue_check(&usdt(dec!(97.5)), &tag("ord-1-payout"))
            .await
            .unwrap();
        assert_eq!(out, IssueOutcome::InsufficientFunds, "{platform:?}");
        assert!(e.world.checks_created_by(e.ub).is_empty());
        assert_eq!(e.balance(platform), dec!(10));
    }
}

#[tokio::test(start_paused = true)]
async fn amount_timeout_after_effect_is_resolved_by_the_same_random_id() {
    let e = env();
    e.fund(CB, dec!(100));
    e.transport.fault_on_text("40", Fault::TimeoutAfterEffect);
    let out = e
        .wallet(CB)
        .issue_check(&usdt(dec!(40)), &tag("ord-1-payout"))
        .await
        .unwrap();
    assert!(matches!(out, IssueOutcome::Issued { .. }), "{out:?}");
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1);
    assert_eq!(
        e.sent_texts(Chat::WalletBot(CB))
            .iter()
            .filter(|t| *t == "40")
            .count(),
        1,
        "повтор тем же random_id не создал второго сообщения"
    );
}

#[tokio::test(start_paused = true)]
async fn check_press_timeout_takes_the_screen_from_the_stream() {
    for platform in [CB, XR] {
        let e = env();
        e.fund(platform, dec!(100));
        e.world
            .inject(e.ub, Call::CheckPress, Fault::TimeoutAfterEffect, 1);
        let out = e
            .wallet(platform)
            .issue_check(&usdt(dec!(30)), &tag("ord-1-payout"))
            .await
            .unwrap();
        assert!(
            matches!(out, IssueOutcome::Issued { .. }),
            "{platform:?}: {out:?}"
        );
        assert_eq!(e.world.checks_created_by(e.ub).len(), 1);
        assert_eq!(e.balance(platform), dec!(70));
    }
}

#[tokio::test(start_paused = true)]
async fn unknown_issue_is_never_retried_and_find_recovers_the_check() {
    for platform in [CB, XR] {
        let e = env();
        e.fund(platform, dec!(100));
        let t = tag("ord-9-payout");
        // Чек создан нажатием, экран чека пришёл позже, чем ждёт сценарий.
        e.world.inject(
            e.ub,
            Call::CheckPress,
            Fault::DelayReply(Duration::from_secs(40)),
            1,
        );
        let wallet = e.wallet(platform);
        let out = wallet.issue_check(&usdt(dec!(25)), &t).await.unwrap();
        assert!(matches!(out, IssueOutcome::Unknown { .. }), "{out:?}");
        assert_eq!(
            e.world.checks_created_by(e.ub).len(),
            1,
            "без второго нажатия и без второго чека"
        );
        tokio::time::sleep(Duration::from_secs(30)).await;
        // История на время недоступна — сверка откладывается, а не выдумывает ответ.
        e.transport
            .fault_on_recent(Chat::SavedMessages, Fault::Disconnected);
        assert!(matches!(
            wallet.find_issued_check(&usdt(dec!(25)), &t).await,
            Err(BotError::Unavailable(_))
        ));
        let found = wallet.find_issued_check(&usdt(dec!(25)), &t).await.unwrap();
        let created = &e.world.checks_created_by(e.ub)[0];
        assert_eq!(
            found.map(|c| c.param),
            Some(created.code.clone()),
            "{platform:?}"
        );
        assert_eq!(e.balance(platform), dec!(75));
    }
}

#[tokio::test(start_paused = true)]
async fn check_press_without_reply_is_unknown_and_left_to_a_human() {
    let e = env();
    e.fund(CB, dec!(100));
    e.world.inject(e.ub, Call::CheckPress, Fault::NoReply, 1);
    let t = tag("ord-1-payout");
    let wallet = e.wallet(CB);
    let out = wallet.issue_check(&usdt(dec!(40)), &t).await.unwrap();
    assert!(matches!(out, IssueOutcome::Unknown { .. }), "{out:?}");
    // Чек есть, но бот о нём не сообщил: связать его с операцией нечем — решает оператор.
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1);
    assert_eq!(
        wallet.find_issued_check(&usdt(dec!(40)), &t).await.unwrap(),
        None
    );
}

#[tokio::test(start_paused = true)]
async fn amount_without_reply_is_unknown_when_the_amount_itself_creates_the_check() {
    let e = env();
    // На мультичек не хватает: чек создаёт сама сумма.
    e.fund(CB, dec!(50));
    e.transport.fault_on_text("40", Fault::NoReply);
    let out = e
        .wallet(CB)
        .issue_check(&usdt(dec!(40)), &tag("ord-1-payout"))
        .await
        .unwrap();
    assert!(matches!(out, IssueOutcome::Unknown { .. }), "{out:?}");
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn xrocket_confirmation_with_another_amount_is_never_pressed() {
    let e = env();
    e.fund(XR, dec!(100));
    // Бот понял сумму не так: на экране подтверждения 11 вместо 10.
    e.transport.fault_on_text(
        "10",
        Fault::UnknownReply(
            "🏷 Персональный чек\n\nСумма чека: 11\u{a0}USDT (11.0$)\n\nПожалуйста, подтвердите корректность данных:"
                .to_owned(),
        ),
    );
    let out = e
        .wallet(XR)
        .issue_check(&usdt(dec!(10)), &tag("ord-1-payout"))
        .await;
    assert!(matches!(out, Err(BotError::Unavailable(_))), "{out:?}");
    assert!(e.world.checks_created_by(e.ub).is_empty());
    assert_eq!(e.balance(XR), dec!(100));
}

#[tokio::test(start_paused = true)]
async fn missing_menu_is_not_executed() {
    let e = env();
    e.fund(CB, dec!(100));
    e.transport.fault_on_text("/checks", Fault::NoReply);
    let out = e
        .wallet(CB)
        .issue_check(&usdt(dec!(10)), &tag("ord-1-payout"))
        .await;
    assert!(matches!(out, Err(BotError::Unavailable(_))), "{out:?}");
    assert!(e.world.checks_created_by(e.ub).is_empty());
}

#[tokio::test(start_paused = true)]
async fn find_tells_operations_apart_by_markers() {
    for platform in [CB, XR] {
        let e = env();
        e.fund(platform, dec!(100));
        let wallet = e.wallet(platform);
        let (t1, t2) = (tag("ord-1-payout"), tag("ord-2-payout"));
        let amount = usdt(dec!(10));
        let IssueOutcome::Issued { check: c1, .. } =
            wallet.issue_check(&amount, &t1).await.unwrap()
        else {
            panic!()
        };
        let IssueOutcome::Issued { check: c2, .. } =
            wallet.issue_check(&amount, &t2).await.unwrap()
        else {
            panic!()
        };
        assert_ne!(c1.param, c2.param);
        assert_eq!(
            wallet.find_issued_check(&amount, &t1).await.unwrap(),
            Some(c1),
            "{platform:?}"
        );
        assert_eq!(
            wallet.find_issued_check(&amount, &t2).await.unwrap(),
            Some(c2)
        );
        assert_eq!(
            wallet
                .find_issued_check(&amount, &tag("ord-3-payout"))
                .await
                .unwrap(),
            None,
            "нет метки — нет чека"
        );
        assert_eq!(
            wallet
                .find_issued_check(&usdt(dec!(11)), &t1)
                .await
                .unwrap(),
            None,
            "другая сумма"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn flood_wait_before_issuing_creates_nothing() {
    let e = env();
    e.fund(XR, dec!(100));
    e.world.inject(
        e.ub,
        Call::Press,
        Fault::FloodWait(Duration::from_secs(61)),
        1,
    );
    let out = e
        .wallet(XR)
        .issue_check(&usdt(dec!(10)), &tag("ord-1-payout"))
        .await;
    assert_eq!(out, Err(BotError::FloodWait(Duration::from_secs(61))));
    assert!(e.world.checks_created_by(e.ub).is_empty());
}

// ============================== счета ==============================

fn shop_invoice(e: &Env, platform: Platform, amount: Money) -> String {
    e.world
        .create_invoice(InvoiceSpec::crypto(platform, amount).by(e.shop))
        .unwrap()
        .code
}

#[tokio::test(start_paused = true)]
async fn inspect_reads_the_invoice_card() {
    let e = env();
    for platform in [CB, XR] {
        let code = shop_invoice(&e, platform, usdt(dec!(10)));
        let info = e.wallet(platform).inspect_invoice(&code).await.unwrap();
        assert_eq!(info.param, code);
        assert_eq!(
            info.card.status,
            parsers::InvoiceStatus::Active,
            "{platform:?}"
        );
        assert_eq!(info.card.amount, Some((dec!(10), Asset::Usdt)));
        assert!(info.card.number.is_some());
        assert!(!info.card.insufficient_funds);
    }
}

#[tokio::test(start_paused = true)]
async fn cryptobot_invoice_is_paid_without_passcode_trusting_a_new_recipient_once() {
    let e = env();
    e.fund(CB, dec!(25));
    let code = shop_invoice(&e, CB, usdt(dec!(10)));
    let wallet = e.wallet(CB);
    let out = wallet
        .pay_invoice(
            &code,
            Asset::Usdt,
            &PayDue::exact(usdt(dec!(10))),
            &tag("ord-1-pay"),
        )
        .await
        .unwrap();
    assert!(matches!(out, PayOutcome::Paid { .. }), "{out:?}");
    let inv = e.world.invoice(&code).unwrap();
    assert_eq!(inv.status, SimInvoiceStatus::Paid);
    assert_eq!(inv.payments.len(), 1);
    assert_eq!(e.balance(CB), dec!(15));
    assert_eq!(e.world.balance(e.shop, CB, Asset::Usdt), dec!(10));
    // Знакомый получатель — без вопроса о доверии.
    let code = shop_invoice(&e, CB, usdt(dec!(5)));
    let out = wallet
        .pay_invoice(
            &code,
            Asset::Usdt,
            &PayDue::exact(usdt(dec!(5))),
            &tag("ord-2-pay"),
        )
        .await
        .unwrap();
    assert!(matches!(out, PayOutcome::Paid { .. }), "{out:?}");
    assert_eq!(e.balance(CB), dec!(10));
}

#[tokio::test(start_paused = true)]
async fn passcode_request_pays_nothing_and_goes_to_a_human() {
    let e = env();
    e.fund(CB, dec!(25));
    // Лимит без код-пароля меньше счёта: CryptoBot спросит код. Код мы не вводим никогда.
    e.world
        .set_passcode_free_limit(e.ub, Some(dec!(5)))
        .unwrap();
    let code = shop_invoice(&e, CB, usdt(dec!(10)));
    let out = e
        .wallet(CB)
        .pay_invoice(
            &code,
            Asset::Usdt,
            &PayDue::exact(usdt(dec!(10))),
            &tag("ord-1-pay"),
        )
        .await
        .unwrap();
    let PayOutcome::NeedsHuman { reason } = &out else {
        panic!("ждали NeedsHuman: {out:?}");
    };
    assert!(reason.contains("passcode"), "{reason}");
    assert_eq!(e.balance(CB), dec!(25));
    assert_eq!(
        e.world.invoice(&code).unwrap().status,
        SimInvoiceStatus::Active
    );
}

#[tokio::test(start_paused = true)]
async fn xrocket_invoice_is_paid_by_one_press() {
    let e = env();
    e.fund(XR, dec!(25));
    let code = shop_invoice(&e, XR, usdt(dec!(10)));
    let wallet = e.wallet(XR);
    let out = wallet
        .pay_invoice(
            &code,
            Asset::Usdt,
            &PayDue::exact(usdt(dec!(10))),
            &tag("ord-1-pay"),
        )
        .await
        .unwrap();
    assert!(matches!(out, PayOutcome::Paid { .. }), "{out:?}");
    assert_eq!(e.world.invoice(&code).unwrap().payments.len(), 1);
    assert_eq!(e.balance(XR), dec!(15));

    // Повторная операция по тому же счёту: «уже оплачен», второй оплаты нет.
    let out = wallet
        .pay_invoice(
            &code,
            Asset::Usdt,
            &PayDue::exact(usdt(dec!(10))),
            &tag("ord-2-pay"),
        )
        .await
        .unwrap();
    assert_eq!(out, PayOutcome::AlreadyPaid);
    assert_eq!(e.world.invoice(&code).unwrap().payments.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn pay_press_timeout_is_never_pressed_again() {
    for platform in [CB, XR] {
        let e = env();
        e.fund(platform, dec!(50));
        let paid = shop_invoice(&e, platform, usdt(dec!(10)));
        e.world
            .inject(e.ub, Call::PayPress, Fault::TimeoutAfterEffect, 1);
        let out = e
            .wallet(platform)
            .pay_invoice(
                &paid,
                Asset::Usdt,
                &PayDue::exact(usdt(dec!(10))),
                &tag("ord-1-pay"),
            )
            .await
            .unwrap();
        // CryptoBot: таймаут на «Оплатить», которое показало вопрос о доверии; дальше —
        // одно нажатие «Отправить платеж».
        assert!(
            matches!(out, PayOutcome::Paid { .. }),
            "{platform:?}: ответ бота пришёл: {out:?}"
        );
        assert_eq!(e.world.invoice(&paid).unwrap().payments.len(), 1);

        // Нажатие сработало, а бот промолчал: исход неизвестен, второго нажатия нет.
        let silent = shop_invoice(&e, platform, usdt(dec!(10)));
        e.world.inject(e.ub, Call::PayPress, Fault::NoReply, 1);
        let out = e
            .wallet(platform)
            .pay_invoice(
                &silent,
                Asset::Usdt,
                &PayDue::exact(usdt(dec!(10))),
                &tag("ord-2-pay"),
            )
            .await
            .unwrap();
        assert!(
            matches!(out, PayOutcome::Unknown { .. }),
            "{platform:?}: {out:?}"
        );
        let payments = e.world.invoice(&silent).unwrap().payments.len();
        assert!(payments <= 1, "{platform:?}");
        assert_eq!(
            e.balance(platform),
            dec!(50) - dec!(10) * Decimal::from(1 + payments)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn invoice_problems_are_reported_without_paying() {
    for platform in [CB, XR] {
        let e = env();
        e.fund(platform, dec!(5));
        let wallet = e.wallet(platform);
        let pay = |code: String, expected: Money, key: &'static str| {
            let wallet = &wallet;
            async move {
                wallet
                    .pay_invoice(&code, Asset::Usdt, &PayDue::exact(expected), &tag(key))
                    .await
                    .unwrap()
            }
        };

        // На счёте другая сумма: ничего не нажато, исход — «сумма изменилась» с суммой счёта.
        let code = shop_invoice(&e, platform, usdt(dec!(10)));
        assert_eq!(
            pay(code.clone(), usdt(dec!(9)), "ord-1-pay").await,
            PayOutcome::AmountChanged {
                due: usdt(dec!(10))
            },
            "{platform:?}: другая сумма"
        );
        assert_eq!(
            pay(code, usdt(dec!(10)), "ord-2-pay").await,
            PayOutcome::InsufficientFunds,
            "{platform:?}"
        );

        let expired = shop_invoice(&e, platform, usdt(dec!(1)));
        e.world.expire_invoice(&expired).unwrap();
        assert_eq!(
            pay(expired, usdt(dec!(1)), "ord-3-pay").await,
            PayOutcome::Expired,
            "{platform:?}"
        );
        assert!(
            matches!(
                pay("IVAAAAAAAAAA".to_owned(), usdt(dec!(1)), "ord-4-pay").await,
                PayOutcome::NeedsHuman { .. }
            ) || platform == XR
        );
        assert_eq!(e.balance(platform), dec!(5));
    }
}

#[tokio::test(start_paused = true)]
async fn fiat_invoice_is_checked_against_the_amount_on_the_coin_button() {
    let e = env();
    e.fund(CB, dec!(50));
    e.world.set_fiat_rate(Asset::Usdt, "USD", dec!(1));
    let code = e
        .world
        .create_invoice(InvoiceSpec::fiat(CB, dec!(10), "USD", &[Asset::Usdt]).by(e.shop))
        .unwrap()
        .code;
    let wallet = e.wallet(CB);
    let info = wallet.inspect_invoice(&code).await.unwrap();
    assert_eq!(info.card.fiat, Some((dec!(10), "USD".to_owned())));
    assert_eq!(info.card.amount, Some((dec!(10), Asset::Usdt)));
    // Курс сдвинулся: на кнопке уже 10.11 вместо 10, а платить можно ровно 10 — не платим.
    e.world.set_fiat_rate(Asset::Usdt, "USD", dec!(0.99));
    let out = wallet
        .pay_invoice(
            &code,
            Asset::Usdt,
            &PayDue::exact(usdt(dec!(10))),
            &tag("ord-1-pay"),
        )
        .await
        .unwrap();
    assert_eq!(
        out,
        PayOutcome::AmountChanged {
            due: usdt(dec!(10.11))
        }
    );
    assert_eq!(e.balance(CB), dec!(50));
    // С запасом на курс до 10.20 — платим то, что просит бот.
    let within = PayDue {
        expected: usdt(dec!(10)),
        min: usdt(dec!(9.9)),
        max: usdt(dec!(10.2)),
    };
    let out = wallet
        .pay_invoice(&code, Asset::Usdt, &within, &tag("ord-3-pay"))
        .await
        .unwrap();
    assert_eq!(
        out,
        PayOutcome::Paid {
            amount: usdt(dec!(10.11))
        }
    );
    assert_eq!(e.balance(CB), dec!(39.89));
    e.world.set_fiat_rate(Asset::Usdt, "USD", dec!(1));
    let code = e
        .world
        .create_invoice(InvoiceSpec::fiat(CB, dec!(10), "USD", &[Asset::Usdt]).by(e.shop))
        .unwrap()
        .code;
    e.world.set_fiat_rate(Asset::Usdt, "USD", dec!(1));
    let out = wallet
        .pay_invoice(
            &code,
            Asset::Usdt,
            &PayDue::exact(usdt(dec!(10))),
            &tag("ord-2-pay"),
        )
        .await
        .unwrap();
    assert!(matches!(out, PayOutcome::Paid { .. }), "{out:?}");
    assert_eq!(e.balance(CB), dec!(29.89), "второй счёт — ровно 10");
}

// ============================== баланс ==============================

#[tokio::test(start_paused = true)]
async fn balances_come_from_the_wallet_screen() {
    let e = env();
    e.world
        .set_balance(e.ub, CB, Asset::Usdt, dec!(12.5))
        .unwrap();
    e.world.set_balance(e.ub, CB, Asset::Ton, dec!(3)).unwrap();
    let balances = e.wallet(CB).balances().await.unwrap();
    assert!(balances.contains(&usdt(dec!(12.5))), "{balances:?}");
    assert!(balances.contains(&ton(dec!(3))), "{balances:?}");
}

// ============================== повторный вызов той же операции ==============================

#[tokio::test(start_paused = true)]
async fn issuing_again_with_the_same_tag_never_creates_a_second_check() {
    for platform in [CB, XR] {
        let e = env();
        e.fund(platform, dec!(100));
        let t = tag("ord-1-payout-1");
        let wallet = e.wallet(platform);
        let IssueOutcome::Issued { check, .. } =
            wallet.issue_check(&usdt(dec!(10)), &t).await.unwrap()
        else {
            panic!("first call issues")
        };
        let again = wallet.issue_check(&usdt(dec!(10)), &t).await.unwrap();
        match again {
            IssueOutcome::Issued { check: same, .. } => assert_eq!(same.param, check.param),
            other => panic!("{other:?}"),
        }
        assert_eq!(e.world.checks_created_by(e.ub).len(), 1, "{platform:?}");
        assert_eq!(e.balance(platform), dec!(90));
    }
}

#[tokio::test(start_paused = true)]
async fn repeating_an_unknown_issue_reconciles_instead_of_issuing() {
    let e = env();
    e.fund(CB, dec!(100));
    let t = tag("ord-2-payout-1");
    let wallet = e.wallet(CB);
    e.world.inject(e.ub, Call::CheckPress, Fault::NoReply, 1);
    let first = wallet.issue_check(&usdt(dec!(40)), &t).await.unwrap();
    assert!(matches!(first, IssueOutcome::Unknown { .. }), "{first:?}");
    // Повтор той же операции ничего не создаёт: бот так и не сообщил о чеке — исход неизвестен.
    let again = wallet.issue_check(&usdt(dec!(40)), &t).await.unwrap();
    assert!(matches!(again, IssueOutcome::Unknown { .. }), "{again:?}");
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1);
    assert_eq!(e.balance(CB), dec!(60));
}

#[tokio::test(start_paused = true)]
async fn xrocket_menu_edit_answers_the_amount_and_frees_the_chat() {
    let e = env();
    e.fund(XR, dec!(100));
    let wallet = e.wallet(XR);
    // Ответ xRocket на сумму — правка меню, а не новое сообщение: следующая операция в этом
    // чате не должна ждать «ответа» на сумму.
    let out = wallet
        .issue_check(&usdt(dec!(10)), &tag("ord-1-payout-1"))
        .await
        .unwrap();
    assert!(matches!(out, IssueOutcome::Issued { .. }));
    let url = e.world.client_check(XR, usdt(dec!(3))).unwrap();
    let started = tokio::time::Instant::now();
    let got = wallet
        .activate_check(param(&url), &tag("ord-2-intake-1"))
        .await
        .unwrap();
    assert_eq!(got, ActivationOutcome::Received(usdt(dec!(3))));
    assert!(
        started.elapsed() < FlowConfig::default().late_reply_quarantine,
        "чат свободен сразу"
    );
}

#[tokio::test(start_paused = true)]
async fn paying_again_with_the_same_tag_never_presses_twice() {
    let e = env();
    e.fund(XR, dec!(25));
    let code = shop_invoice(&e, XR, usdt(dec!(10)));
    let t = tag("ord-1-pay-1");
    let wallet = e.wallet(XR);
    let first = wallet
        .pay_invoice(&code, Asset::Usdt, &PayDue::exact(usdt(dec!(10))), &t)
        .await
        .unwrap();
    assert!(matches!(first, PayOutcome::Paid { .. }), "{first:?}");
    let again = wallet
        .pay_invoice(&code, Asset::Usdt, &PayDue::exact(usdt(dec!(10))), &t)
        .await
        .unwrap();
    assert!(matches!(again, PayOutcome::Unknown { .. }), "{again:?}");
    assert_eq!(e.world.invoice(&code).unwrap().payments.len(), 1);
    assert_eq!(e.balance(XR), dec!(15));
}

// ============================== окно ответа (находки проверки денежного ядра) ==============================

/// A промолчал, B выиграл 300, затем повтор A с тем же `random_id`: ответ B лежит в истории
/// после команды A, но вне её окна (после команды B) — A не должен его забрать.
#[tokio::test(start_paused = true)]
async fn retried_activation_never_takes_the_next_orders_reply() {
    let e = env();
    let wallet = e.wallet(CB);
    let a = e.world.client_check(CB, usdt(dec!(5))).unwrap();
    let ta = tag("ord-1-intake-1");
    e.world.inject(e.ub, Call::SendText, Fault::NoReply, 1);
    let out = wallet.activate_check(param(&a), &ta).await.unwrap();
    assert_eq!(out, ActivationOutcome::Unknown { raw: None });

    // B: чат занят неотвеченной командой A — отказ, пока человек не откроет чат; затем проходит.
    let b = e.world.client_check(CB, usdt(dec!(300))).unwrap();
    let tb = tag("ord-2-intake-1");
    assert!(matches!(
        wallet.activate_check(param(&b), &tb).await,
        Err(BotError::Unavailable(_))
    ));
    // Сам чат не освобождается и через `unanswered_block` (аудит 11.11.2026, Ю-2): его
    // открывает человек, разобрав заявку A.
    tokio::time::advance(FlowConfig::default().unanswered_block).await;
    assert!(matches!(
        wallet.activate_check(param(&b), &tb).await,
        Err(BotError::ChatBlocked)
    ));
    wallet.acknowledge_unanswered().await;
    let out = wallet.activate_check(param(&b), &tb).await.unwrap();
    assert_eq!(out, ActivationOutcome::Received(usdt(dec!(300))));

    let retry = wallet.activate_check(param(&a), &ta).await.unwrap();
    assert!(
        matches!(retry, ActivationOutcome::Unknown { .. }),
        "A must not adopt B's reply: {retry:?}"
    );
    assert_eq!(
        e.balance(CB),
        dec!(305),
        "both checks are really in the wallet"
    );
}

/// Ответ на A пришёл через 55 с — позже таймаута и карантина. B ждёт его и не забирает себе;
/// повтор A находит свой ответ в своём окне.
#[tokio::test(start_paused = true)]
async fn very_late_reply_is_kept_out_of_the_next_operation() {
    let e = env();
    let wallet = e.wallet(CB);
    let a = e.world.client_check(CB, usdt(dec!(300))).unwrap();
    let ta = tag("ord-1-intake-1");
    e.world.inject(
        e.ub,
        Call::SendText,
        Fault::DelayReply(Duration::from_secs(55)),
        1,
    );
    let out = wallet.activate_check(param(&a), &ta).await.unwrap();
    assert!(matches!(out, ActivationOutcome::Unknown { .. }), "{out:?}");

    let b = e.world.client_check(CB, usdt(dec!(2))).unwrap();
    let out = wallet
        .activate_check(param(&b), &tag("ord-2-intake-1"))
        .await
        .unwrap();
    assert_eq!(
        out,
        ActivationOutcome::Received(usdt(dec!(2))),
        "B gets its own reply"
    );

    let retry = wallet.activate_check(param(&a), &ta).await.unwrap();
    assert_eq!(retry, ActivationOutcome::Received(usdt(dec!(300))));
    assert_eq!(e.balance(CB), dec!(302));
}

// ============================== молчание кошелька дольше блока (аудит 11.11.2026, Ю-2) =============

/// Бот кошелька молчал полчаса. Пока активация A без ответа, B в чат не уходит — ни сразу, ни
/// через `unanswered_block`: поздний ответ на A несёт сумму и достался бы B. Ответ пришёл —
/// каждая заявка получает свою сумму.
#[tokio::test(start_paused = true)]
async fn unanswered_activation_keeps_the_chat_until_its_reply_arrives() {
    let e = env();
    let wallet = e.wallet(CB);
    let a = e.world.client_check(CB, usdt(dec!(300))).unwrap();
    let ta = tag("ord-1-intake-1");
    e.world.inject(
        e.ub,
        Call::SendText,
        Fault::DelayReply(Duration::from_secs(1800)),
        1,
    );
    let out = wallet.activate_check(param(&a), &ta).await.unwrap();
    assert!(matches!(out, ActivationOutcome::Unknown { .. }), "{out:?}");

    let b = e.world.client_check(CB, usdt(dec!(2))).unwrap();
    let tb = tag("ord-2-intake-1");
    assert!(matches!(
        wallet.activate_check(param(&b), &tb).await,
        Err(BotError::Unavailable(_))
    ));
    tokio::time::advance(FlowConfig::default().unanswered_block).await;
    let blocked = wallet.activate_check(param(&b), &tb).await;
    assert!(
        matches!(blocked, Err(BotError::ChatBlocked)),
        "B must not be sent while A has no reply: {blocked:?}"
    );

    // Ответ на A пришёл: чат чист без участия человека.
    tokio::time::advance(Duration::from_secs(1300)).await;
    let retry = wallet.activate_check(param(&a), &ta).await.unwrap();
    assert_eq!(retry, ActivationOutcome::Received(usdt(dec!(300))));
    let out = wallet.activate_check(param(&b), &tb).await.unwrap();
    assert_eq!(out, ActivationOutcome::Received(usdt(dec!(2))));
    assert_eq!(e.balance(CB), dec!(302));
}

/// Человек открыл чат, хотя ответ на A так и не пришёл, а потом бот ответил на A и B подряд.
/// Два итоговых ответа в окне B — исход неясен: ни одна заявка не получает чужую сумму.
#[tokio::test(start_paused = true)]
async fn late_backlog_after_a_human_cleared_the_chat_is_ambiguous_not_misattributed() {
    let e = env();
    let wallet = e.wallet(CB);
    let a = e.world.client_check(CB, usdt(dec!(300))).unwrap();
    let ta = tag("ord-1-intake-1");
    e.world.inject(
        e.ub,
        Call::SendText,
        Fault::DelayReply(Duration::from_secs(1800)),
        1,
    );
    let out = wallet.activate_check(param(&a), &ta).await.unwrap();
    assert!(matches!(out, ActivationOutcome::Unknown { .. }), "{out:?}");
    let b = e.world.client_check(CB, usdt(dec!(2))).unwrap();
    let tb = tag("ord-2-intake-1");
    tokio::time::advance(FlowConfig::default().unanswered_block).await;
    assert!(matches!(
        wallet.activate_check(param(&b), &tb).await,
        Err(BotError::ChatBlocked)
    ));

    wallet.acknowledge_unanswered().await;
    // Ответ на B бот отдаст сразу после ответа на A (очередь после молчания).
    e.world.inject(
        e.ub,
        Call::SendText,
        Fault::DelayReply(Duration::from_secs(1210)),
        1,
    );
    let out = wallet.activate_check(param(&b), &tb).await.unwrap();
    assert!(matches!(out, ActivationOutcome::Unknown { .. }), "{out:?}");

    tokio::time::advance(Duration::from_secs(1400)).await;
    let retry_b = wallet.activate_check(param(&b), &tb).await.unwrap();
    assert!(
        matches!(retry_b, ActivationOutcome::Unknown { .. }),
        "B must not adopt A's 300: {retry_b:?}"
    );
    let retry_a = wallet.activate_check(param(&a), &ta).await.unwrap();
    assert!(
        matches!(retry_a, ActivationOutcome::Unknown { .. }),
        "A must not adopt B's 2: {retry_a:?}"
    );
    assert_eq!(e.balance(CB), dec!(302), "both checks are in the wallet");
}

/// Ответ на A потерян навсегда, человек разобрал заявку и открыл чат: B получает свой ответ.
#[tokio::test(start_paused = true)]
async fn after_a_human_cleared_the_chat_the_next_activation_goes_through() {
    let e = env();
    let wallet = e.wallet(CB);
    let a = e.world.client_check(CB, usdt(dec!(5))).unwrap();
    let ta = tag("ord-1-intake-1");
    e.world.inject(e.ub, Call::SendText, Fault::NoReply, 1);
    let out = wallet.activate_check(param(&a), &ta).await.unwrap();
    assert_eq!(out, ActivationOutcome::Unknown { raw: None });
    let b = e.world.client_check(CB, usdt(dec!(7))).unwrap();
    let tb = tag("ord-2-intake-1");
    tokio::time::advance(FlowConfig::default().unanswered_block).await;
    assert!(matches!(
        wallet.activate_check(param(&b), &tb).await,
        Err(BotError::ChatBlocked)
    ));
    wallet.acknowledge_unanswered().await;
    let out = wallet.activate_check(param(&b), &tb).await.unwrap();
    assert_eq!(out, ActivationOutcome::Received(usdt(dec!(7))));
}
