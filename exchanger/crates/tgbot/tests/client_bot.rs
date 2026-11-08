//! Клиентский бот целиком: поддельный Bot API на локальном TCP, движок с юзерботом на
//! симуляторе кошельков, живой PostgreSQL. Клиент пишет боту ссылку на чек — бот присылает
//! одно сообщение заявки и правит его до выплаты; после рестарта бота сообщение догоняет
//! состояние заявки.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use botapi::BotApi;
use domain::fsm::OrderState;
use domain::ledger::{self, AccountCode};
use domain::{Asset, Decimal, Direction, Money, Platform};
use engine::{Engine, EngineConfig};
use rust_decimal_macros::dec;
use secrecy::SecretString;
use serde_json::{Value, json};
use sqlx::PgPool;
use storage::ledger::PostingRefs;
use tgbot::{ClientBot, ClientBotConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use ui::{EmojiSet, Ui, UiConfig};
use userbot::flows::FlowConfig;
use userbot::sim::{AccountId, InvoiceSpec, SimInvoiceStatus, SimWorld};
use userbot::{AccountGate, UserbotWallet, WalletBot};

const CB: Platform = Platform::CryptoBot;
const XR: Platform = Platform::XRocket;
const CLIENT: i64 = 111;
const TOKEN: &str = "123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw";

fn usdt(x: Decimal) -> Money {
    Money::new(x, Asset::Usdt).unwrap()
}

// ---------------------------------------------------------------------------------------------
// Поддельный Bot API.

#[derive(Debug, Clone)]
struct Call {
    method: String,
    body: Value,
}

impl Call {
    fn chat(&self) -> Option<i64> {
        self.body["chat_id"].as_i64()
    }

    fn message_id(&self) -> Option<i64> {
        self.body["message_id"].as_i64()
    }

    /// Текст и клавиатура запроса одной строкой: искать ссылки и номера заявок.
    fn contains(&self, needle: &str) -> bool {
        self.body.to_string().contains(needle)
    }
}

#[derive(Default)]
struct State {
    updates: Vec<Value>,
    next_update: i64,
    next_message: i64,
    calls: Vec<Call>,
    /// Сообщения клиента в их текущем виде (правки обновляют).
    messages: HashMap<i64, Value>,
    /// Ответы бота: его сообщение → на какое сообщение клиента ответ.
    replies: HashMap<i64, i64>,
    /// Отдаёт ли Telegram кнопки сообщения в `reply_to_message` (проверяется вживую).
    no_markup_in_replies: bool,
}

impl State {
    /// Сообщение клиента для `reply_to_message` — в текущем виде.
    fn replied(&self, id: i64) -> Value {
        let mut m = self.messages.get(&id).cloned().unwrap_or_else(
            || json!({ "message_id": id, "date": 0, "chat": { "id": CLIENT, "type": "private" } }),
        );
        if self.no_markup_in_replies
            && let Some(o) = m.as_object_mut()
        {
            o.remove("reply_markup");
        }
        m
    }
}

/// Bot API: отдаёт очередь обновлений и запоминает все прочие вызовы.
#[derive(Clone)]
struct FakeTelegram {
    base: String,
    state: Arc<Mutex<State>>,
}

impl FakeTelegram {
    async fn start() -> (Self, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fake = Self {
            base: format!("http://{}", listener.local_addr().unwrap()),
            state: Arc::new(Mutex::new(State {
                next_update: 1,
                next_message: 1000,
                ..State::default()
            })),
        };
        let server = fake.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let server = server.clone();
                tokio::spawn(async move {
                    let Some((method, body)) = read_request(&mut socket).await else {
                        return;
                    };
                    let result = server.respond(&method, body).await;
                    let payload = json!({ "ok": true, "result": result }).to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    socket.write_all(response.as_bytes()).await.ok();
                    socket.shutdown().await.ok();
                });
            }
        });
        (fake, handle)
    }

    fn api(&self) -> BotApi {
        BotApi::builder(SecretString::from(TOKEN))
            .base_url(&self.base)
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    /// Клиент пишет боту текст.
    fn client_writes(&self, text: &str) {
        let mut s = self.lock();
        let id = s.next_update;
        s.next_update += 1;
        s.updates.push(json!({
            "update_id": id,
            "message": {
                "message_id": id,
                "date": 0,
                "chat": { "id": CLIENT, "type": "private", "first_name": "Client" },
                "from": { "id": CLIENT, "is_bot": false, "first_name": "Client", "language_code": "ru" },
                "text": text,
            }
        }));
    }

    /// Клиент пересылает сообщение бота-магазина: текст без ссылки, ссылка — в кнопке
    /// `{"text": …, "url": …}` или `{"text": …, "web_app": {"url": …}}`.
    fn client_forwards(&self, text: &str, button: Value) {
        let mut s = self.lock();
        let id = s.next_update;
        s.next_update += 1;
        s.updates.push(json!({
            "update_id": id,
            "message": {
                "message_id": id,
                "date": 0,
                "chat": { "id": CLIENT, "type": "private", "first_name": "Client" },
                "from": { "id": CLIENT, "is_bot": false, "first_name": "Client", "language_code": "ru" },
                "forward_origin": { "type": "user", "date": 0,
                    "sender_user": { "id": 777, "is_bot": true, "first_name": "Shop", "username": "shop_bot" } },
                "text": text,
                "reply_markup": { "inline_keyboard": [[ button ]] },
            }
        }));
    }

    /// Сообщение клиента в чате с ботом (`kind`: `message` или `edited_message`) с
    /// дополнительными полями (`via_bot`, `reply_markup`, `edit_date`).
    fn client_message(&self, kind: &str, message_id: i64, text: &str, extra: Value) {
        let mut s = self.lock();
        let id = s.next_update;
        s.next_update += 1;
        let mut message = json!({
            "message_id": message_id,
            "date": 1,
            "chat": { "id": CLIENT, "type": "private", "first_name": "Client" },
            "from": { "id": CLIENT, "is_bot": false, "first_name": "Client", "language_code": "ru" },
            "text": text,
        });
        if let (Some(m), Some(e)) = (message.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                m.insert(k.clone(), v.clone());
            }
        }
        s.messages.insert(message_id, message.clone());
        s.updates.push(json!({ "update_id": id, kind: message }));
    }

    /// Кошелёк поправил сообщение клиента, но Telegram правку боту не доставил.
    fn client_message_edited_silently(&self, message_id: i64, text: &str, extra: Value) {
        let mut s = self.lock();
        let Some(m) = s.messages.get_mut(&message_id) else {
            return;
        };
        m["text"] = json!(text);
        if let Some(e) = extra.as_object() {
            for (k, v) in e {
                m[k] = v.clone();
            }
        }
    }

    /// Telegram не кладёт кнопки в `reply_to_message` — остаётся служебная пересылка.
    fn without_markup_in_replies(&self) {
        self.lock().no_markup_in_replies = true;
    }

    /// Клиент нажимает инлайн-кнопку `data` под сообщением `message_id`.
    fn client_presses(&self, message_id: i64, data: &str) {
        let mut s = self.lock();
        let id = s.next_update;
        s.next_update += 1;
        s.updates.push(json!({
            "update_id": id,
            "callback_query": {
                "id": format!("cb{id}"),
                "from": { "id": CLIENT, "is_bot": false, "first_name": "Client" },
                "message": {
                    "message_id": message_id,
                    "date": 0,
                    "chat": { "id": CLIENT, "type": "private" },
                },
                "chat_instance": "1",
                "data": data,
            }
        }));
    }

    fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    fn calls_of(&self, method: &str) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| c.method == method)
            .collect()
    }

    /// Ждать вызова, который подходит под `pred`.
    async fn wait_for(&self, what: &str, pred: impl Fn(&Call) -> bool) -> Call {
        for _ in 0..500 {
            if let Some(call) = self.calls().into_iter().find(&pred) {
                return call;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("no {what}; calls: {:#?}", self.calls());
    }

    async fn respond(&self, method: &str, body: Value) -> Value {
        if method == "getUpdates" {
            let offset = body["offset"].as_i64().unwrap_or(0);
            // Как Telegram: запрос с `offset` подтверждает всё, что ниже, — больше не придёт.
            self.lock()
                .updates
                .retain(|u| u["update_id"].as_i64().unwrap() >= offset);
            // Короткий long polling: до 200 мс ждём новые обновления.
            for _ in 0..20 {
                let ready: Vec<Value> = self
                    .lock()
                    .updates
                    .iter()
                    .filter(|u| u["update_id"].as_i64().unwrap() >= offset)
                    .cloned()
                    .collect();
                if !ready.is_empty() {
                    return Value::Array(ready);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            return json!([]);
        }
        let mut s = self.lock();
        s.calls.push(Call {
            method: method.to_owned(),
            body: body.clone(),
        });
        match method {
            "getMe" => json!({
                "id": 999, "is_bot": true, "first_name": "Exch", "username": "exch_test_bot"
            }),
            "sendMessage" | "sendRichMessage" => {
                s.next_message += 1;
                let id = s.next_message;
                let mut m = json!({
                    "message_id": id,
                    "date": 0,
                    "chat": { "id": body["chat_id"], "type": "private" },
                    "text": body["text"],
                });
                if let Some(to) = body["reply_parameters"]["message_id"].as_i64() {
                    s.replies.insert(id, to);
                    m["reply_to_message"] = s.replied(to);
                }
                m
            }
            "editMessageText" => {
                let mut m = json!({
                    "message_id": body["message_id"],
                    "date": 0,
                    "chat": { "id": body["chat_id"], "type": "private" },
                    "text": body["text"],
                });
                if let Some(to) = body["message_id"]
                    .as_i64()
                    .and_then(|id| s.replies.get(&id).copied())
                {
                    m["reply_to_message"] = s.replied(to);
                }
                m
            }
            "forwardMessage" => {
                s.next_message += 1;
                let id = s.next_message;
                let mut m = body["message_id"]
                    .as_i64()
                    .and_then(|m| s.messages.get(&m).cloned())
                    .unwrap_or_else(|| json!({}));
                m["message_id"] = json!(id);
                m["chat"] = json!({ "id": body["chat_id"], "type": "private" });
                m["forward_origin"] = json!({ "type": "user", "date": 0,
                    "sender_user": { "id": CLIENT, "is_bot": false, "first_name": "Client" } });
                m
            }
            _ => json!(true),
        }
    }
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Option<(String, Value)> {
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let n = socket.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8(buf[..header_end].to_vec()).ok()?;
    let mut lines = head.split("\r\n");
    // POST /bot<token>/<method> HTTP/1.1
    let path = lines.next()?.split(' ').nth(1)?.to_owned();
    let method = path.rsplit('/').next()?.to_owned();
    let mut content_length = 0;
    for line in lines {
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().ok()?;
        }
    }
    while buf.len() < header_end + content_length {
        let n = socket.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = if content_length == 0 {
        json!({})
    } else {
        serde_json::from_slice(&buf[header_end..header_end + content_length]).ok()?
    };
    Some((method, body))
}

// ---------------------------------------------------------------------------------------------
// Движок на симуляторе: два кошелька по 1000 USDT, направления открыты.

struct Env {
    pool: PgPool,
    world: SimWorld,
    ub: AccountId,
    client: AccountId,
    engine: Arc<Engine>,
    fake: FakeTelegram,
}

fn fast() -> FlowConfig {
    FlowConfig {
        reply_timeout: Duration::from_millis(300),
        min_interval: Duration::ZERO,
        bot_actions_per_minute: 10_000,
        late_reply_quarantine: Duration::from_millis(100),
        unanswered_block: Duration::from_millis(500),
        xrocket_not_found_retry: Some(Duration::from_millis(10)),
        ..FlowConfig::default()
    }
}

async fn env(pool: PgPool) -> Env {
    let world = SimWorld::new();
    let ub = world.add_account("exch_ub");
    let client = world.add_account("client");
    let mut tx = pool.begin().await.unwrap();
    let ub_id = storage::wallets::ensure_userbot(&mut tx, "ub-1", "+00******01")
        .await
        .unwrap();
    for platform in [CB, XR] {
        storage::wallets::ensure_wallet(&mut tx, platform, ub_id)
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();

    let mut engine = Engine::new(
        pool.clone(),
        EngineConfig {
            unavailable_backoff: Duration::ZERO,
            activation_retry_backoff: Duration::ZERO,
            reconcile_grace: Duration::ZERO,
            ..EngineConfig::default()
        },
    );
    engine.ensure_ledger_accounts().await.unwrap();

    let mut tx = pool.begin().await.unwrap();
    for platform in [CB, XR] {
        let capital = ledger::capital_in(platform, Asset::Usdt, dec!(1000)).unwrap();
        storage::ledger::ensure_account(
            &mut tx,
            &AccountCode::equity(ledger::Equity::Capital, Asset::Usdt),
            Asset::Usdt,
            None,
        )
        .await
        .unwrap();
        storage::ledger::post(
            &mut tx,
            &capital,
            PostingRefs {
                memo: Some("initial capital"),
                created_by: Some("staff:1"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        world
            .set_balance(ub, platform, Asset::Usdt, dec!(1000))
            .unwrap();
    }
    for d in Direction::ALL {
        storage::directions::set_open(&mut tx, d, true)
            .await
            .unwrap();
    }
    storage::settings::set(
        &mut tx,
        "daily_limit_usd",
        &json!({ "default": 1_000_000, "new_user": 1_000_000 }),
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let transport = Arc::new(world.transport(ub));
    let gate = Arc::new(AccountGate::new("ub-1"));
    let mut wallets: HashMap<Platform, Arc<dyn WalletBot>> = HashMap::new();
    for platform in [CB, XR] {
        let w = Arc::new(
            UserbotWallet::new(platform, transport.clone(), gate.clone()).with_config(fast()),
        );
        wallets.insert(platform, w.clone() as Arc<dyn WalletBot>);
        engine = engine.with_wallet(w);
    }
    let (fake, _server) = FakeTelegram::start().await;
    Env {
        pool,
        world,
        ub,
        client,
        engine: Arc::new(engine),
        fake,
    }
}

struct Running {
    cancel: CancellationToken,
    task: JoinHandle<Result<(), tgbot::TgBotError>>,
}

impl Running {
    async fn stop(self) {
        self.cancel.cancel();
        tokio::time::timeout(Duration::from_secs(20), self.task)
            .await
            .expect("бот остановился")
            .unwrap()
            .unwrap();
    }
}

impl Env {
    fn start_bot(&self) -> Running {
        self.start_bot_with(false)
    }

    /// Бот с rich-сообщениями (справочные экраны — карточками).
    fn start_bot_with(&self, rich_messages: bool) -> Running {
        let ui = Ui::builtin(EmojiSet::plain(), UiConfig::default()).unwrap();
        let cfg = ClientBotConfig {
            poll_timeout_secs: 0,
            rich_messages,
            shutdown_grace: Duration::from_secs(5),
            check_link_step: Duration::from_millis(100),
            check_link_wait: Duration::from_secs(3),
            check_link_forward_at: vec![Duration::from_millis(600)],
            ..ClientBotConfig::default()
        };
        let bot = ClientBot::new(
            self.fake.api(),
            ui,
            self.engine.clone(),
            self.pool.clone(),
            cfg,
        );
        let cancel = CancellationToken::new();
        let task = tokio::spawn({
            let cancel = cancel.clone();
            async move { bot.run(cancel).await }
        });
        Running { cancel, task }
    }

    async fn drain(&self) {
        for _ in 0..20 {
            if self.engine.process_due().await.unwrap() == 0 {
                return;
            }
        }
        panic!("operations keep coming");
    }

    /// Заявки клиента: (id, состояние, чат и сообщение заявки, ссылка на чек выплаты).
    async fn orders(&self) -> Vec<(i64, OrderState, Option<i64>, Option<i32>, Option<String>)> {
        let rows: Vec<(i64, Option<i64>, Option<i32>)> = sqlx::query_as(
            "SELECT id, progress_chat_id, progress_message_id FROM orders WHERE user_id = $1 ORDER BY id",
        )
        .bind(CLIENT)
        .fetch_all(&self.pool)
        .await
        .unwrap();
        let mut conn = self.pool.acquire().await.unwrap();
        let mut out = Vec::new();
        for (id, chat, message) in rows {
            let order = storage::orders::get(&mut conn, id, false).await.unwrap();
            let details = storage::orders::details(&mut conn, id).await.unwrap();
            out.push((id, order.state, chat, message, details.payout_check_url));
        }
        out
    }

    /// Единственная заявка клиента и её сообщение — когда бот записал id сообщения в БД
    /// (фейковый сервер видит `sendMessage` раньше, чем бот получает ответ).
    async fn order_message(&self) -> (i64, i64, String) {
        for _ in 0..500 {
            let row: Option<(i64, Option<i32>, String)> = sqlx::query_as(
                "SELECT id, progress_message_id, public_id FROM orders WHERE user_id = $1 ORDER BY id LIMIT 1",
            )
            .bind(CLIENT)
            .fetch_optional(&self.pool)
            .await
            .unwrap();
            if let Some((id, Some(message), public_id)) = row {
                return (id, i64::from(message), public_id);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("order message id was never stored");
    }

    async fn ledger(&self, code: &str) -> Decimal {
        sqlx::query_scalar::<_, Option<Decimal>>(
            "SELECT SUM(e.amount) FROM ledger_entries e JOIN ledger_accounts a ON a.id = e.account_id WHERE a.code = $1",
        )
        .bind(code)
        .fetch_one(&self.pool)
        .await
        .unwrap()
        .unwrap_or_default()
    }

    fn calls_contain(&self, needle: &str) -> bool {
        self.fake.calls().iter().any(|c| c.contains(needle))
    }

    async fn assert_books_match(&self) {
        assert_eq!(
            self.ledger("asset:cb:USDT").await,
            self.world.balance(self.ub, CB, Asset::Usdt),
            "CryptoBot"
        );
        assert_eq!(
            self.ledger("asset:xr:USDT").await,
            self.world.balance(self.ub, XR, Asset::Usdt),
            "xRocket"
        );
    }
}

// ---------------------------------------------------------------------------------------------

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn check_link_gets_one_message_that_ends_with_the_payout(pool: PgPool) {
    let e = env(pool).await;
    let bot = e.start_bot();
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    e.fake.client_writes(&format!("Вот чек: {url}"));

    let sent = e
        .fake
        .wait_for("order message", |c| {
            c.method == "sendMessage" && c.chat() == Some(CLIENT)
        })
        .await;
    let (order_id, message, _) = e.order_message().await;
    let orders = e.orders().await;
    assert_eq!(orders.len(), 1, "одна заявка");
    assert_eq!(orders[0].2, Some(CLIENT), "чат сообщения заявки сохранён");

    e.drain().await;
    let (_, state, _, _, payout_url) = e.orders().await[0].clone();
    assert_eq!(state, OrderState::Completed);
    let payout_url = payout_url.expect("ссылка на чек выплаты");
    e.fake
        .wait_for("edit with the payout check", |c| {
            c.method == "editMessageText" && c.contains(&payout_url)
        })
        .await;
    bot.stop().await;

    // Всё — в одном сообщении: новых сообщений нет, правится то, что ушло при приёме.
    let sends = e.fake.calls_of("sendMessage");
    assert_eq!(sends.len(), 1, "одно сообщение на заявку: {sends:#?}");
    assert!(!sent.contains(&url), "ссылку клиента бот не повторяет");
    for edit in e.fake.calls_of("editMessageText") {
        assert_eq!(edit.chat(), Some(CLIENT));
        assert_eq!(
            edit.message_id(),
            Some(message),
            "правится сообщение заявки {order_id}"
        );
    }

    // Деньги: клиент забирает чек xRocket, книги сходятся.
    e.world.claim_check(e.client, &payout_url).unwrap();
    assert_eq!(e.world.balance(e.client, XR, Asset::Usdt), dec!(97.5));
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1);
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn the_same_check_twice_is_one_order(pool: PgPool) {
    let e = env(pool).await;
    let bot = e.start_bot();
    let url = e.world.client_check(CB, usdt(dec!(40))).unwrap();
    e.fake.client_writes(&url);
    e.fake.client_writes(&url);

    e.fake
        .wait_for("duplicate notice", |c| {
            c.method == "sendMessage" && c.contains("уже в работе")
        })
        .await;
    let orders = e.orders().await;
    assert_eq!(orders.len(), 1, "второй раз тот же чек — не новая заявка");

    e.drain().await;
    let (_, state, _, _, payout_url) = e.orders().await[0].clone();
    assert_eq!(state, OrderState::Completed);
    let payout_url = payout_url.unwrap();
    e.fake
        .wait_for("edit with the payout check", |c| {
            c.method == "editMessageText" && c.contains(&payout_url)
        })
        .await;
    bot.stop().await;
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1, "одна выплата");
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn after_a_restart_the_order_message_catches_up(pool: PgPool) {
    let e = env(pool).await;
    let bot = e.start_bot();
    let url = e.world.client_check(XR, usdt(dec!(50))).unwrap();
    e.fake.client_writes(&url);
    e.fake
        .wait_for("order message", |c| {
            c.method == "sendMessage" && c.chat() == Some(CLIENT)
        })
        .await;
    bot.stop().await;

    // Пока бот лежит, движок доводит заявку до выплаты.
    e.drain().await;
    let (_, state, _, message, payout_url) = e.orders().await[0].clone();
    assert_eq!(state, OrderState::Completed);
    let payout_url = payout_url.unwrap();
    assert!(
        e.fake
            .calls_of("editMessageText")
            .iter()
            .all(|c| !c.contains(&payout_url)),
        "бот был остановлен"
    );

    let bot = e.start_bot();
    let edit = e
        .fake
        .wait_for("catch-up edit", |c| {
            c.method == "editMessageText" && c.contains(&payout_url)
        })
        .await;
    assert_eq!(edit.message_id(), message.map(i64::from));
    bot.stop().await;
    assert_eq!(
        e.fake.calls_of("sendMessage").len(),
        1,
        "нового сообщения нет"
    );
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn text_without_a_link_gets_a_hint_and_no_order(pool: PgPool) {
    let e = env(pool).await;
    let bot = e.start_bot();
    e.fake.client_writes("привет");
    e.fake
        .wait_for("reply", |c| {
            c.method == "sendMessage" && c.chat() == Some(CLIENT)
        })
        .await;
    bot.stop().await;
    assert!(e.orders().await.is_empty());
    assert!(e.world.checks_created_by(e.ub).is_empty());
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn invoice_link_gets_a_quote_and_the_check_pays_it(pool: PgPool) {
    let e = env(pool).await;
    let bot = e.start_bot();
    let shop = e.world.add_account("shop");
    let inv = e
        .world
        .create_invoice(InvoiceSpec::crypto(CB, usdt(dec!(10))).by(shop))
        .unwrap();
    e.fake.client_writes(&inv.url);

    let quote = e
        .fake
        .wait_for("quote", |c| {
            c.method == "sendMessage" && c.contains("Пришлите чек") && c.contains("10.30")
        })
        .await;
    let (_, message, public_id) = e.order_message().await;
    assert_eq!(e.orders().await[0].1, OrderState::AwaitingFunds);
    // Макет B1: сумма к оплате — на синей кнопке-копии, «Отмена» отменяет заявку.
    assert!(quote.contains("\"copy_text\""), "{:?}", quote.body);
    assert!(
        quote.contains(&format!("cancel:{public_id}")),
        "{:?}",
        quote.body
    );
    // Старое сообщение с «Пришлю чек» по-прежнему работает: экран ожидания чека.
    e.fake
        .client_presses(message, &format!("pay_check:{public_id}"));
    e.fake
        .wait_for("awaiting", |c| {
            c.method == "editMessageText"
                && c.message_id() == Some(message)
                && c.contains("Жду чек")
        })
        .await;

    // Чек xRocket идёт в эту заявку, счёт оплачивается.
    let check = e.world.client_check(XR, usdt(dec!(10.30))).unwrap();
    e.fake.client_writes(&check);
    e.fake
        .wait_for("checking", |c| {
            c.method == "editMessageText"
                && c.message_id() == Some(message)
                && !c.contains("Жду чек")
        })
        .await;
    e.drain().await;
    e.fake
        .wait_for("paid", |c| {
            c.method == "editMessageText"
                && c.message_id() == Some(message)
                && c.contains("Счёт оплачен")
        })
        .await;
    bot.stop().await;

    assert_eq!(e.orders().await.len(), 1, "чек не стал отдельной заявкой");
    assert_eq!(
        e.fake.calls_of("sendMessage").len(),
        1,
        "одно сообщение на заявку"
    );
    let paid = e.world.invoice(&inv.code).unwrap();
    assert_eq!(paid.status, SimInvoiceStatus::Paid);
    assert_eq!(paid.payments.len(), 1);
    e.assert_books_match().await;
}

/// Магазин присылает счёт кнопкой «Оплатить» (обычная ссылка, мини-приложение CryptoBot,
/// веб-версия счёта); клиент пересылает сообщение — бот берёт счёт из кнопки.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn invoice_in_a_forwarded_shop_button_gets_a_quote(pool: PgPool) {
    let e = env(pool).await;
    let bot = e.start_bot();
    let shop = e.world.add_account("shop");
    let inv = e
        .world
        .create_invoice(InvoiceSpec::crypto(CB, usdt(dec!(10))).by(shop))
        .unwrap();
    e.fake.client_forwards(
        "Пополнение баланса на 10 USDT. Нажмите «Оплатить».",
        json!({
            "text": "Оплатить",
            "web_app": { "url": format!("https://t.me/CryptoBot/app?startapp=invoice-{}&mode=compact", inv.code) }
        }),
    );
    e.fake
        .wait_for("quote", |c| {
            c.method == "sendMessage" && c.contains("Пришлите чек") && c.contains("10.30")
        })
        .await;
    bot.stop().await;
    let orders = e.orders().await;
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0].1, OrderState::AwaitingFunds);
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn xrocket_invoice_in_a_url_button_is_read_too(pool: PgPool) {
    let e = env(pool).await;
    let bot = e.start_bot();
    let shop = e.world.add_account("shop");
    let inv = e
        .world
        .create_invoice(InvoiceSpec::crypto(XR, usdt(dec!(20))).by(shop))
        .unwrap();
    e.fake.client_forwards(
        "Счёт на оплату",
        json!({ "text": "💳 Оплатить", "url": inv.url }),
    );
    e.fake
        .wait_for("quote", |c| {
            c.method == "sendMessage" && c.contains("Пришлите чек") && c.contains("20.40")
        })
        .await;
    bot.stop().await;
    assert_eq!(e.orders().await.len(), 1);
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn cancel_button_closes_the_quote(pool: PgPool) {
    let e = env(pool).await;
    let bot = e.start_bot();
    let shop = e.world.add_account("shop");
    let inv = e
        .world
        .create_invoice(InvoiceSpec::crypto(XR, usdt(dec!(20))).by(shop))
        .unwrap();
    e.fake.client_writes(&inv.url);
    e.fake
        .wait_for("quote", |c| {
            c.method == "sendMessage" && c.contains("Пришлите чек") && c.contains("20.40")
        })
        .await;
    let (_, message, public_id) = e.order_message().await;
    e.fake
        .client_presses(message, &format!("cancel:{public_id}"));
    e.fake
        .wait_for("cancelled", |c| {
            c.method == "editMessageText"
                && c.message_id() == Some(message)
                && c.contains("отменена")
        })
        .await;
    bot.stop().await;
    assert_eq!(e.orders().await[0].1, OrderState::Cancelled);
    assert_eq!(
        e.world.invoice(&inv.code).unwrap().status,
        SimInvoiceStatus::Active,
        "счёт не тронут"
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn xrocket_multi_check_is_refused_and_never_opened(pool: PgPool) {
    let e = env(pool).await;
    let bot = e.start_bot();
    let multi = e
        .world
        .create_check(userbot::sim::CheckSpec::new(XR, usdt(dec!(5))).multi(2))
        .unwrap();
    e.fake
        .client_writes(&format!("Держи мульти-чек {}", multi.url));
    e.fake
        .wait_for("multi-check refusal", |c| {
            c.method == "sendMessage" && c.contains("Мульти-чек не подойдёт")
        })
        .await;
    bot.stop().await;
    assert!(e.orders().await.is_empty());
    assert!(
        e.world
            .messages(e.ub, userbot::Chat::WalletBot(XR))
            .iter()
            .all(|m| !m.out),
        "юзербот ничего не отправлял боту xRocket"
    );
    assert!(e.world.check(&multi.code).unwrap().claimed_by.is_empty());
}

// ---------------------------------------------------------------------------------------------
// Инвестиции (DESIGN-v0.2 Р18)

/// Выдать клиенту доступ к инвестициям: доля 50 %, без блокировки, автовывод до 100 USDT.
async fn grant_investor(e: &Env) {
    let mut conn = e.pool.acquire().await.unwrap();
    let owner = storage::staff::ensure_owner(&mut conn, 1, "Owner")
        .await
        .unwrap();
    storage::users::upsert(&mut conn, CLIENT, &storage::users::Profile::default())
        .await
        .unwrap();
    drop(conn);
    e.engine
        .invest_save_terms(
            &owner,
            &storage::invest::Investor {
                user_id: CLIENT,
                share_pct: dec!(50),
                lock_days: 0,
                min_deposit: dec!(10),
                auto_withdraw_max: dec!(100),
                is_active: true,
                granted_by: None,
                created_at: chrono::Utc::now(),
            },
        )
        .await
        .unwrap();
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn the_invest_section_is_hidden_from_regular_clients(pool: PgPool) {
    let e = env(pool).await;
    let bot = e.start_bot();
    e.fake.client_writes("/start");
    let welcome = e
        .fake
        .wait_for("welcome", |c| c.method == "sendMessage")
        .await;
    assert!(
        !welcome.contains("Инвестиции"),
        "кнопки нет у обычного клиента"
    );
    e.fake.client_writes("💼 Инвестиции");
    e.fake
        .wait_for("hint", |c| {
            c.method == "sendMessage" && c.contains("Пришлите чек")
        })
        .await;
    e.fake.client_presses(1000, "inv:dep");
    tokio::time::sleep(Duration::from_millis(300)).await;
    bot.stop().await;
    assert!(
        !e.calls_contain("Пополнение"),
        "чужая кнопка раздела ничего не открывает"
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn investor_deposits_by_check_and_withdraws_a_check(pool: PgPool) {
    let e = env(pool).await;
    grant_investor(&e).await;
    let bot = e.start_bot();

    e.fake.client_writes("/start");
    e.fake
        .wait_for("menu with the invest button", |c| {
            c.method == "sendMessage" && c.contains("Инвестиции")
        })
        .await;
    e.fake.client_writes("💼 Инвестиции");
    e.fake
        .wait_for("invest home", |c| {
            c.method == "sendMessage" && c.contains("В обороте")
        })
        .await;

    // Пополнить → чек → зачислено.
    e.fake.client_presses(1001, "inv:dep");
    e.fake
        .wait_for("deposit prompt", |c| c.contains("Пополнение"))
        .await;
    let url = e.world.client_check(CB, usdt(dec!(200))).unwrap();
    e.fake.client_writes(&url);
    e.fake
        .wait_for("deposit accepted", |c| c.contains("Чек принят"))
        .await;
    e.drain().await;
    e.fake
        .wait_for("deposit credited", |c| {
            c.method == "sendMessage" && c.contains("Зачислено") && c.contains("200.00 USDT")
        })
        .await;
    assert!(
        e.orders().await.is_empty(),
        "чек пополнения — не заявка обмена"
    );

    // Вывести → xRocket → 50 → чек.
    e.fake.client_presses(1001, "inv:to:xrocket");
    e.fake
        .wait_for("amount prompt", |c| c.contains("Сколько вывести"))
        .await;
    e.fake.client_writes("50");
    e.fake
        .wait_for("withdrawal started", |c| c.contains("Создаю чек"))
        .await;
    e.drain().await;
    let checks = e.world.checks_created_by(e.ub);
    assert_eq!(checks.len(), 1, "один чек вывода");
    let check_url = checks[0].url.clone();
    e.fake
        .wait_for("withdrawal check", |c| {
            c.method == "sendMessage" && c.contains(&check_url) && c.contains("готов")
        })
        .await;
    bot.stop().await;

    e.world.claim_check(e.client, &check_url).unwrap();
    assert_eq!(e.world.balance(e.client, XR, Asset::Usdt), dec!(50));
    assert_eq!(
        e.ledger(&format!("liability:investor:{CLIENT}:USDT")).await,
        dec!(-150)
    );
    e.assert_books_match().await;
}

// ---------------------------------------------------------------------------------------------
// Чек со ссылкой в кнопке

fn via_send() -> Value {
    json!({ "id": 1_559_501_630_i64, "is_bot": true, "first_name": "Crypto Bot", "username": "send" })
}

/// Главный путь из макета: клиент набирает `@send 100usdt` прямо в чате с ботом. CryptoBot
/// присылает заглушку «Создание чека…» без ссылки, а ссылку добавляет кнопкой правкой того же
/// сообщения. Бот ждёт, берёт чек из правки, и повторная правка не создаёт вторую заявку.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn inline_check_created_in_the_chat_is_taken_from_the_edited_button(pool: PgPool) {
    let e = env(pool).await;
    let bot = e.start_bot();
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();

    e.fake.client_message(
        "message",
        50,
        "Создание чека…",
        json!({ "via_bot": via_send() }),
    );
    e.fake
        .wait_for("pending notice", |c| {
            c.method == "sendMessage" && c.contains("дописывает ссылку")
        })
        .await;
    assert!(e.orders().await.is_empty(), "без ссылки заявки нет");
    assert!(
        !e.calls_contain("Пришлите чек"),
        "не подсказка «пришлите чек»"
    );

    let button = json!({ "inline_keyboard": [[{ "text": "Получить 100 USDT", "url": url }]] });
    e.fake.client_message(
        "edited_message",
        50,
        "🦋 Чек на 100 USDT",
        json!({ "via_bot": via_send(), "edit_date": 2, "reply_markup": button }),
    );
    let (order_id, _, _) = e.order_message().await;
    e.drain().await;
    let (_, state, _, _, payout_url) = e.orders().await[0].clone();
    assert_eq!(state, OrderState::Completed, "заявка {order_id} выплачена");
    let payout_url = payout_url.unwrap();

    // Кошелёк правит сообщение ещё раз (например, «чек активирован») — второй заявки нет.
    e.fake.client_message(
        "edited_message",
        50,
        "🦋 Чек на 100 USDT активирован",
        json!({ "via_bot": via_send(), "edit_date": 3, "reply_markup": button }),
    );
    e.fake
        .wait_for("edit with the payout check", |c| {
            c.method == "editMessageText" && c.contains(&payout_url)
        })
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    bot.stop().await;
    assert_eq!(e.orders().await.len(), 1, "одна заявка на чек");
    assert!(!e.calls_contain("уже в работе"), "повторная правка — молча");
    e.world.claim_check(e.client, &payout_url).unwrap();
    e.assert_books_match().await;
}

/// Ссылка на чек только в кнопке «Скопировать» (`copy_text`) пересланного сообщения.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn check_link_in_a_copy_button_is_taken(pool: PgPool) {
    let e = env(pool).await;
    let bot = e.start_bot();
    let url = e.world.client_check(XR, usdt(dec!(30))).unwrap();
    e.fake.client_message(
        "message",
        60,
        "Чек на 30 USDT",
        json!({
            "forward_origin": { "type": "user", "date": 0,
                "sender_user": { "id": 777, "is_bot": false, "first_name": "Friend" } },
            "reply_markup": { "inline_keyboard": [[{ "text": "Скопировать ссылку", "copy_text": { "text": url } }]] },
        }),
    );
    e.order_message().await;
    e.drain().await;
    let (_, state, _, _, _) = e.orders().await[0].clone();
    assert_eq!(state, OrderState::Completed);
    bot.stop().await;
}

/// Клиент поправил своё сообщение без ссылки — бот молчит (не «пришлите чек» второй раз).
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn edited_text_without_a_link_is_ignored(pool: PgPool) {
    let e = env(pool).await;
    let bot = e.start_bot();
    e.fake.client_message("message", 70, "привет", json!({}));
    e.fake
        .wait_for("hint", |c| {
            c.method == "sendMessage" && c.contains("Пришлите чек")
        })
        .await;
    e.fake
        .client_message("edited_message", 70, "привет!", json!({ "edit_date": 2 }));
    tokio::time::sleep(Duration::from_millis(400)).await;
    bot.stop().await;
    assert_eq!(
        e.fake.calls_of("sendMessage").len(),
        1,
        "на правку без ссылки — тишина"
    );
}

/// Клиент видит, почему направление на паузе: «в xRocket закончился резерв», а не голое «пауза».
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn paused_direction_tells_the_client_why(pool: PgPool) {
    let e = env(pool).await;
    sqlx::query(
        "UPDATE directions SET min_amount = 2000, max_amount = 5000 WHERE code = 'cb_to_xr_check'",
    )
    .execute(&e.pool)
    .await
    .unwrap();
    let bot = e.start_bot();
    e.fake.client_writes("Обменять чек");
    e.fake
        .wait_for("exchange menu with the reason", |c| {
            c.method == "sendMessage" && c.contains("в xRocket закончился резерв")
        })
        .await;
    bot.stop().await;
}

/// Telegram правку боту не доставил: бот сам перечитывает сообщение с чеком ответом-цитатой
/// и берёт ссылку, клиент ничего не делает.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn check_link_is_read_from_the_reply_when_the_edit_never_arrives(pool: PgPool) {
    let e = env(pool).await;
    let bot = e.start_bot();
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    e.fake.client_message(
        "message",
        80,
        "Создание чека…",
        json!({ "via_bot": via_send() }),
    );
    e.fake
        .wait_for("waiting reply", |c| {
            c.method == "sendMessage" && c.body["reply_parameters"]["message_id"] == 80
        })
        .await;
    e.fake.client_message_edited_silently(
        80,
        "🦋 Чек на 100 USDT",
        json!({ "reply_markup": { "inline_keyboard": [[{ "text": "Получить", "url": url }]] } }),
    );
    let (order_id, _, _) = e.order_message().await;
    e.fake
        .wait_for("waiting message removed", |c| c.method == "deleteMessage")
        .await;
    e.drain().await;
    let (_, state, _, _, _) = e.orders().await[0].clone();
    assert_eq!(state, OrderState::Completed, "заявка {order_id}");
    bot.stop().await;
    assert!(!e.calls_contain("ерешлите"), "клиента ни о чём не просим");
    assert!(
        e.fake.calls_of("forwardMessage").is_empty(),
        "хватило ответа-цитаты"
    );
}

/// В ответе-цитате кнопок нет — бот берёт ссылку служебной пересылкой и удаляет копию.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn check_link_is_read_by_a_service_forward_as_a_last_resort(pool: PgPool) {
    let e = env(pool).await;
    {
        let mut conn = e.pool.acquire().await.unwrap();
        storage::settings::set(&mut conn, "service_chat", &json!({ "id": 4242 }), None)
            .await
            .unwrap();
    }
    e.fake.without_markup_in_replies();
    let bot = e.start_bot();
    let url = e.world.client_check(XR, usdt(dec!(40))).unwrap();
    e.fake.client_message(
        "message",
        81,
        "Создание чека…",
        json!({ "via_bot": { "id": 5_014_831_088_i64, "is_bot": true, "first_name": "xRocket", "username": "xrocket" } }),
    );
    e.fake.client_message_edited_silently(
        81,
        "Чек на 40 USDT",
        json!({ "reply_markup": { "inline_keyboard": [[{ "text": "Получить", "url": url }]] } }),
    );
    e.order_message().await;
    let fwd = e
        .fake
        .wait_for("service forward", |c| c.method == "forwardMessage")
        .await;
    assert_eq!(fwd.body["chat_id"], 4242);
    assert_eq!(fwd.body["disable_notification"], true);
    e.fake
        .wait_for("service copy deleted", |c| {
            c.method == "deleteMessage" && c.body["chat_id"] == 4242
        })
        .await;
    e.drain().await;
    assert_eq!(e.orders().await[0].1, OrderState::Completed);
    bot.stop().await;
}

/// Ссылка так и не появилась: клиенту — «чек не открыт, деньги в нём», владельцу — событие.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn missing_check_link_tells_where_the_money_is(pool: PgPool) {
    let e = env(pool).await;
    let bot = e.start_bot();
    e.fake.client_message(
        "message",
        82,
        "Создание чека…",
        json!({ "via_bot": via_send() }),
    );
    e.fake
        .wait_for("missing link notice", |c| {
            c.method == "editMessageText" && c.contains("так и не прислал ссылку")
        })
        .await;
    bot.stop().await;
    assert!(e.orders().await.is_empty());
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM alerts WHERE title = 'check_link_missing' AND acked_at IS NULL",
    )
    .fetch_one(&e.pool)
    .await
    .unwrap();
    assert_eq!(n, 1, "владельцу — событие с устройством сообщения");
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn plain_view_switches_back_to_the_rich_card_in_the_same_message(pool: PgPool) {
    let e = env(pool).await;
    let bot = e.start_bot_with(true);
    e.fake.client_writes("/terms");
    e.fake
        .wait_for("rich terms", |c| {
            c.method == "sendRichMessage" && c.contains("Курс и лимиты") && c.contains("\"plain\"")
        })
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let message = e.fake.lock().next_message;
    e.fake.client_presses(message, "plain");
    let plain = e
        .fake
        .wait_for("plain edit", |c| {
            c.method == "editMessageText" && c.message_id() == Some(message)
        })
        .await;
    assert!(plain.body["text"].is_string(), "простой вид — обычный HTML");
    assert!(plain.contains("\"rich\"") && plain.contains("Красивый вид"));
    assert!(
        !plain.contains("\"plain\""),
        "кнопки «Простой вид» больше нет"
    );
    e.fake.client_presses(message, "rich");
    let rich = e
        .fake
        .wait_for("rich edit", |c| {
            c.method == "editMessageText"
                && c.message_id() == Some(message)
                && c.body["rich_message"].is_object()
        })
        .await;
    assert!(rich.contains("\"plain\""), "в карточке снова «Простой вид»");
    bot.stop().await;
    assert_eq!(
        e.fake.calls_of("sendRichMessage").len() + e.fake.calls_of("sendMessage").len(),
        1,
        "вид меняется в том же сообщении, новых нет"
    );
}

/// «История» по решению владельца 8 ноября: время в строке — кнопка в тексте (callback, как в
/// гаранте); нажали — карточка заявки, без «/start» в чате.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn history_row_opens_the_order_card_by_callback(pool: PgPool) {
    let e = env(pool).await;
    let bot = e.start_bot_with(true);
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    e.fake.client_writes(&url);
    let (_, _, public_id) = e.order_message().await;
    e.fake.client_writes("/history");
    let history = e
        .fake
        .wait_for("history", |c| {
            c.method == "sendRichMessage" && c.contains("История")
        })
        .await;
    let data = format!("order:{public_id}");
    assert!(
        history.contains(&format!(
            "tg-button type=\\\"callback_data\\\" style=\\\"link\\\" data=\\\"{data}\\\""
        )),
        "время — кнопка в тексте: {}",
        history.body
    );
    assert!(!history.contains("start=ord_"), "без ссылок со «/start»");
    e.fake.client_presses(1000, &data);
    e.fake
        .wait_for("card", |c| {
            matches!(c.method.as_str(), "sendRichMessage" | "sendMessage")
                && c.contains(&public_id)
                && c.contains("Заявка")
        })
        .await;
    bot.stop().await;
}
