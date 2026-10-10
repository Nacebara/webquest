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
    SimWebAppPayer, SimWorld,
};
use userbot::transport::{
    CallbackAnswer, Chat, InlineResults, RawMessage, Transport, TransportError,
};
use userbot::{
    AccountGate, ActivationOutcome, BotError, IssueMethod, IssueOutcome, OpTag, PayOutcome,
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
    on_press: Mutex<Vec<(String, Fault)>>,
}

impl Scripted {
    fn fault_on_text(&self, text: &str, fault: Fault) {
        self.on_text.lock().unwrap().push((text.to_owned(), fault));
    }

    fn fault_on_press(&self, data_contains: &str, fault: Fault) {
        self.on_press
            .lock()
            .unwrap()
            .push((data_contains.to_owned(), fault));
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
        let data_str = String::from_utf8_lossy(data).into_owned();
        self.arm(&self.on_press, |k| data_str.contains(k), Call::Press);
        self.inner.press(chat, msg_id, data).await
    }

    async fn inline_query(
        &self,
        bot: Platform,
        query: &str,
    ) -> Result<InlineResults, TransportError> {
        self.inner.inline_query(bot, query).await
    }

    async fn send_inline(
        &self,
        to: Chat,
        bot: Platform,
        results: &InlineResults,
        result_id: &str,
        random_id: i64,
    ) -> Result<RawMessage, TransportError> {
        self.inner
            .send_inline(to, bot, results, result_id, random_id)
            .await
    }

    async fn open_webapp(&self, bot: Platform, button_url: &str) -> Result<String, TransportError> {
        self.inner.open_webapp(bot, button_url).await
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
        on_press: Mutex::new(Vec::new()),
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

    fn menu_only(&self, platform: Platform) -> UserbotWallet<Scripted> {
        self.wallet(platform).with_config(FlowConfig {
            inline_checks: false,
            ..FlowConfig::default()
        })
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

    // «Вы получили 100 USDT» придёт во время следующей операции и не будет её ответом,
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
        .find(|l| l.message.text == "Вы получили 100 USDT.")
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

    let url = e.world.client_check(CB, usdt(dec!(2))).unwrap();
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

async fn issue_inline_once(platform: Platform) {
    let e = env();
    e.fund(platform, dec!(500));
    let total = e.world.total_in_world(platform, Asset::Usdt);
    let t = tag("ord-7-payout");
    let out = e
        .wallet(platform)
        .issue_check(&usdt(dec!(97.5)), &t)
        .await
        .unwrap();
    let IssueOutcome::Issued { check, method } = out else {
        panic!("{out:?}");
    };
    assert_eq!(method, IssueMethod::Inline);
    assert_eq!(check.amount, usdt(dec!(97.5)));

    let created = e.world.checks_created_by(e.ub);
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].code, check.param);
    assert_eq!(created[0].url, check.url);
    assert_eq!(
        created[0].origin,
        CheckOrigin::Inline {
            random_id: t.random_id
        }
    );
    assert_eq!(e.balance(platform), dec!(402.5));
    assert_eq!(e.world.total_in_world(platform, Asset::Usdt), total);

    let saved = e.world.messages(e.ub, Chat::SavedMessages);
    assert_eq!(saved[0].text, userbot::flows::marker_text("ord-7-payout"));
    assert_eq!(saved[1].via_bot, Some(platform));

    // Клиент забирает чек — деньги у него.
    e.world.claim_check(e.client, &check.url).unwrap();
    assert_eq!(e.world.balance(e.client, platform, Asset::Usdt), dec!(97.5));
}

#[tokio::test(start_paused = true)]
async fn inline_check_is_created_once_xrocket() {
    issue_inline_once(XR).await;
}

#[tokio::test(start_paused = true)]
async fn inline_check_is_created_once_cryptobot() {
    issue_inline_once(CB).await;
}

#[tokio::test(start_paused = true)]
async fn menu_creates_the_check_on_both_platforms() {
    for platform in [CB, XR] {
        let e = env();
        e.fund(platform, dec!(100));
        let out = e
            .menu_only(platform)
            .issue_check(&usdt(dec!(12.25)), &tag("ord-1-payout"))
            .await
            .unwrap();
        let IssueOutcome::Issued { check, method } = out else {
            panic!("{platform:?}: {out:?}");
        };
        assert_eq!(method, IssueMethod::Menu);
        let created = e.world.checks_created_by(e.ub);
        assert_eq!(created.len(), 1, "{platform:?}");
        assert_eq!(created[0].code, check.param);
        assert!(matches!(created[0].origin, CheckOrigin::Menu { .. }));
        assert_eq!(e.balance(platform), dec!(87.75));
    }
}

#[tokio::test(start_paused = true)]
async fn inline_failure_before_sending_falls_back_to_menu() {
    let e = env();
    e.fund(XR, dec!(100));
    e.world.inject(
        e.ub,
        Call::InlineQuery,
        Fault::Rpc {
            code: 400,
            name: "BOT_INLINE_DISABLED".to_owned(),
        },
        1,
    );
    let out = e
        .wallet(XR)
        .issue_check(&usdt(dec!(10)), &tag("ord-1-payout"))
        .await
        .unwrap();
    assert!(
        matches!(
            out,
            IssueOutcome::Issued {
                method: IssueMethod::Menu,
                ..
            }
        ),
        "{out:?}"
    );
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn insufficient_funds_creates_nothing() {
    let e = env();
    e.fund(CB, dec!(10));
    let out = e
        .wallet(CB)
        .issue_check(&usdt(dec!(97.5)), &tag("ord-1-payout"))
        .await
        .unwrap();
    assert_eq!(out, IssueOutcome::InsufficientFunds);
    assert!(e.world.checks_created_by(e.ub).is_empty());
    assert_eq!(e.balance(CB), dec!(10));
}

#[tokio::test(start_paused = true)]
async fn inline_timeout_after_effect_is_resolved_by_the_same_random_id() {
    let e = env();
    e.fund(XR, dec!(100));
    e.world
        .inject(e.ub, Call::SendInline, Fault::TimeoutAfterEffect, 1);
    let out = e
        .wallet(XR)
        .issue_check(&usdt(dec!(30)), &tag("ord-1-payout"))
        .await
        .unwrap();
    assert!(
        matches!(
            out,
            IssueOutcome::Issued {
                method: IssueMethod::Inline,
                ..
            }
        ),
        "{out:?}"
    );
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1);
    assert_eq!(e.balance(XR), dec!(70));
}

#[tokio::test(start_paused = true)]
async fn unknown_issue_is_never_retried_and_find_recovers_the_check() {
    let e = env();
    e.fund(CB, dec!(100));
    let t = tag("ord-9-payout");
    // Чек создан, ответ потерян; повтор упал до сервера; история недоступна.
    e.world
        .inject(e.ub, Call::SendInline, Fault::TimeoutAfterEffect, 1);
    e.world
        .inject(e.ub, Call::SendInline, Fault::Disconnected, 1);
    e.world.inject(e.ub, Call::History, Fault::Disconnected, 1);
    let wallet = e.wallet(CB);
    let out = wallet.issue_check(&usdt(dec!(25)), &t).await.unwrap();
    assert!(matches!(out, IssueOutcome::Unknown { .. }), "{out:?}");
    assert_eq!(
        e.world.checks_created_by(e.ub).len(),
        1,
        "без меню и без второго чека"
    );

    e.world.clear_faults(e.ub);
    let found = wallet.find_issued_check(&usdt(dec!(25)), &t).await.unwrap();
    let created = &e.world.checks_created_by(e.ub)[0];
    assert_eq!(found.map(|c| c.param), Some(created.code.clone()));
    assert_eq!(e.balance(CB), dec!(75));
}

#[tokio::test(start_paused = true)]
async fn menu_amount_timeout_is_resolved_by_the_same_random_id() {
    let e = env();
    e.fund(CB, dec!(100));
    e.transport.fault_on_text("40", Fault::TimeoutAfterEffect);
    let out = e
        .menu_only(CB)
        .issue_check(&usdt(dec!(40)), &tag("ord-1-payout"))
        .await
        .unwrap();
    assert!(
        matches!(
            out,
            IssueOutcome::Issued {
                method: IssueMethod::Menu,
                ..
            }
        ),
        "{out:?}"
    );
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn menu_without_reply_to_amount_is_unknown_and_left_to_a_human() {
    let e = env();
    e.fund(CB, dec!(100));
    e.transport.fault_on_text("40", Fault::NoReply);
    let t = tag("ord-1-payout");
    let wallet = e.menu_only(CB);
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
async fn find_tells_operations_apart_by_markers() {
    let e = env();
    e.fund(XR, dec!(100));
    let wallet = e.wallet(XR);
    let (t1, t2) = (tag("ord-1-payout"), tag("ord-2-payout"));
    let amount = usdt(dec!(10));
    let IssueOutcome::Issued { check: c1, .. } = wallet.issue_check(&amount, &t1).await.unwrap()
    else {
        panic!()
    };
    let IssueOutcome::Issued { check: c2, .. } = wallet.issue_check(&amount, &t2).await.unwrap()
    else {
        panic!()
    };
    assert_ne!(c1.param, c2.param);
    assert_eq!(
        wallet.find_issued_check(&amount, &t1).await.unwrap(),
        Some(c1)
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

#[tokio::test(start_paused = true)]
async fn flood_wait_before_issuing_creates_nothing() {
    let e = env();
    e.fund(XR, dec!(100));
    e.world.inject(
        e.ub,
        Call::InlineQuery,
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
        .create_invoice(
            InvoiceSpec::crypto(platform, amount)
                .by(e.shop)
                .description("Заказ 42"),
        )
        .unwrap()
        .code
}

#[tokio::test(start_paused = true)]
async fn inspect_reads_the_invoice_card() {
    let e = env();
    let code = shop_invoice(&e, CB, usdt(dec!(10)));
    let info = e.wallet(CB).inspect_invoice(&code).await.unwrap();
    assert_eq!(info.param, code);
    assert_eq!(info.card.status, parsers::InvoiceStatus::Active);
    assert_eq!(info.card.amount, Some((dec!(10), Asset::Usdt)));
    assert_eq!(info.card.single_use, Some(true));
    assert_eq!(info.card.description.as_deref(), Some("Заказ 42"));
}

#[tokio::test(start_paused = true)]
async fn cryptobot_invoice_without_pin_step_goes_to_a_human() {
    let e = env();
    e.fund(CB, dec!(25));
    let code = shop_invoice(&e, CB, usdt(dec!(10)));
    let out = e
        .wallet(CB)
        .pay_invoice(&code, Asset::Usdt, &usdt(dec!(10)), &tag("ord-1-pay"))
        .await
        .unwrap();
    assert!(matches!(out, PayOutcome::NeedsHuman { .. }), "{out:?}");
    assert_eq!(e.balance(CB), dec!(25));
    assert_eq!(
        e.world.invoice(&code).unwrap().status,
        SimInvoiceStatus::Active
    );
}

#[tokio::test(start_paused = true)]
async fn cryptobot_invoice_is_paid_through_the_mini_app() {
    let e = env();
    e.fund(CB, dec!(25));
    e.world.set_pin(e.ub, "2580").unwrap();
    let code = shop_invoice(&e, CB, usdt(dec!(10)));
    let wallet = e
        .wallet(CB)
        .with_payer(Arc::new(SimWebAppPayer::new(e.world.clone(), "2580")));
    let out = wallet
        .pay_invoice(&code, Asset::Usdt, &usdt(dec!(10)), &tag("ord-1-pay"))
        .await
        .unwrap();
    assert_eq!(out, PayOutcome::Paid);
    let inv = e.world.invoice(&code).unwrap();
    assert_eq!(inv.status, SimInvoiceStatus::Paid);
    assert_eq!(inv.payments.len(), 1);
    assert_eq!(e.balance(CB), dec!(15));
    assert_eq!(e.world.balance(e.shop, CB, Asset::Usdt), dec!(10));
}

#[tokio::test(start_paused = true)]
async fn wrong_pin_pays_nothing_and_goes_to_a_human() {
    let e = env();
    e.fund(CB, dec!(25));
    e.world.set_pin(e.ub, "2580").unwrap();
    let code = shop_invoice(&e, CB, usdt(dec!(10)));
    let wallet = e
        .wallet(CB)
        .with_payer(Arc::new(SimWebAppPayer::new(e.world.clone(), "0000")));
    let out = wallet
        .pay_invoice(&code, Asset::Usdt, &usdt(dec!(10)), &tag("ord-1-pay"))
        .await
        .unwrap();
    assert!(matches!(out, PayOutcome::NeedsHuman { .. }), "{out:?}");
    assert_eq!(e.balance(CB), dec!(25));
}

#[tokio::test(start_paused = true)]
async fn xrocket_invoice_is_paid_by_one_press() {
    let e = env();
    e.fund(XR, dec!(25));
    let code = shop_invoice(&e, XR, usdt(dec!(10)));
    let wallet = e.wallet(XR);
    let out = wallet
        .pay_invoice(&code, Asset::Usdt, &usdt(dec!(10)), &tag("ord-1-pay"))
        .await
        .unwrap();
    assert_eq!(out, PayOutcome::Paid);
    assert_eq!(e.world.invoice(&code).unwrap().payments.len(), 1);
    assert_eq!(e.balance(XR), dec!(15));

    // Повторная операция по тому же счёту: «уже оплачен», второй оплаты нет.
    let out = wallet
        .pay_invoice(&code, Asset::Usdt, &usdt(dec!(10)), &tag("ord-2-pay"))
        .await
        .unwrap();
    assert_eq!(out, PayOutcome::AlreadyPaid);
    assert_eq!(e.world.invoice(&code).unwrap().payments.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn pay_press_timeout_is_never_pressed_again() {
    let e = env();
    e.fund(XR, dec!(50));
    let paid = shop_invoice(&e, XR, usdt(dec!(10)));
    e.transport
        .fault_on_press(":pay:", Fault::TimeoutAfterEffect);
    let out = e
        .wallet(XR)
        .pay_invoice(&paid, Asset::Usdt, &usdt(dec!(10)), &tag("ord-1-pay"))
        .await
        .unwrap();
    assert_eq!(
        out,
        PayOutcome::Paid,
        "ответ бота пришёл, хотя нажатие вернуло таймаут"
    );
    assert_eq!(e.world.invoice(&paid).unwrap().payments.len(), 1);

    // Нажатие сработало, а бот промолчал: исход неизвестен, второго нажатия нет.
    let silent = shop_invoice(&e, XR, usdt(dec!(10)));
    e.transport.fault_on_press(":pay:", Fault::NoReply);
    let out = e
        .wallet(XR)
        .pay_invoice(&silent, Asset::Usdt, &usdt(dec!(10)), &tag("ord-2-pay"))
        .await
        .unwrap();
    assert!(matches!(out, PayOutcome::Unknown { .. }), "{out:?}");
    assert_eq!(e.world.invoice(&silent).unwrap().payments.len(), 1);
    assert_eq!(e.balance(XR), dec!(30));
}

#[tokio::test(start_paused = true)]
async fn invoice_problems_are_reported_without_paying() {
    let e = env();
    e.fund(XR, dec!(5));
    let wallet = e.wallet(XR);
    let pay = |code: String, expected: Money, key: &'static str| {
        let wallet = &wallet;
        async move {
            wallet
                .pay_invoice(&code, Asset::Usdt, &expected, &tag(key))
                .await
                .unwrap()
        }
    };

    let code = shop_invoice(&e, XR, usdt(dec!(10)));
    assert!(matches!(
        pay(code.clone(), usdt(dec!(9)), "ord-1-pay").await,
        PayOutcome::NeedsHuman { .. }
    ));
    assert_eq!(
        pay(code, usdt(dec!(10)), "ord-2-pay").await,
        PayOutcome::InsufficientFunds
    );

    let expired = shop_invoice(&e, XR, usdt(dec!(1)));
    e.world.expire_invoice(&expired).unwrap();
    assert_eq!(
        pay(expired, usdt(dec!(1)), "ord-3-pay").await,
        PayOutcome::Expired
    );

    let multi = e
        .world
        .create_invoice(
            InvoiceSpec::crypto(XR, usdt(dec!(1)))
                .by(e.shop)
                .multi_use(),
        )
        .unwrap()
        .code;
    assert!(matches!(
        pay(multi, usdt(dec!(1)), "ord-4-pay").await,
        PayOutcome::NeedsHuman { .. }
    ));
    assert_eq!(e.balance(XR), dec!(5));
}

#[tokio::test(start_paused = true)]
async fn fiat_invoice_is_checked_against_the_amount_due() {
    let e = env();
    e.fund(CB, dec!(50));
    e.world.set_pin(e.ub, "1111").unwrap();
    e.world.set_fiat_rate(Asset::Usdt, "USD", dec!(1));
    let code = e
        .world
        .create_invoice(InvoiceSpec::fiat(CB, dec!(10), "USD", &[Asset::Usdt]).by(e.shop))
        .unwrap()
        .code;
    let wallet = e
        .wallet(CB)
        .with_payer(Arc::new(SimWebAppPayer::new(e.world.clone(), "1111")));
    let out = wallet
        .pay_invoice(&code, Asset::Usdt, &usdt(dec!(10)), &tag("ord-1-pay"))
        .await
        .unwrap();
    assert_eq!(out, PayOutcome::Paid);
    assert_eq!(e.balance(CB), dec!(40));
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
