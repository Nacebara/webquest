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
use userbot::sim::{AccountId, InvoiceSpec, SimInvoiceStatus, SimWebAppPayer, SimWorld};
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
                json!({
                    "message_id": s.next_message,
                    "date": 0,
                    "chat": { "id": body["chat_id"], "type": "private" },
                    "text": body["text"],
                })
            }
            "editMessageText" => json!({
                "message_id": body["message_id"],
                "date": 0,
                "chat": { "id": body["chat_id"], "type": "private" },
                "text": body["text"],
            }),
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
    world.set_pin(ub, "2580").unwrap();
    for platform in [CB, XR] {
        let w = Arc::new(
            UserbotWallet::new(platform, transport.clone(), gate.clone())
                .with_config(fast())
                .with_payer(Arc::new(SimWebAppPayer::new(world.clone(), "2580"))),
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
        let ui = Ui::builtin(EmojiSet::plain(), UiConfig::default()).unwrap();
        let cfg = ClientBotConfig {
            poll_timeout_secs: 0,
            rich_messages: false,
            shutdown_grace: Duration::from_secs(5),
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
            c.method == "sendMessage" && c.contains("10.30 USDT")
        })
        .await;
    let (_, message, public_id) = e.order_message().await;
    assert_eq!(e.orders().await[0].1, OrderState::AwaitingFunds);
    assert!(
        quote.contains(&format!("pay_check:{public_id}")),
        "{:?}",
        quote.body
    );

    // «Пришлю чек» → то же сообщение показывает ожидание чека.
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
            c.method == "sendMessage" && c.contains("20.40 USDT")
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
