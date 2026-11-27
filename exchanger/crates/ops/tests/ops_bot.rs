//! Пульт целиком — внутри клиентского бота, как в бою: поддельный Bot API на локальном TCP,
//! клиентский бот с крючком пульта, движок с юзерботом на симуляторе кошельков, живой
//! PostgreSQL. Сотрудник нажимает кнопки и пишет ответы — проверяем, что изменения проходят
//! только через предпросмотр и одноразовое подтверждение, пишутся в журнал, а посторонние
//! пульта не видят.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{CB, XR, accepted, usdt};
use domain::fsm::OrderState;
use domain::{Asset, Direction};
use engine::Engine;
use ops::{OpsBot, OpsConfig};
use rust_decimal_macros::dec;
use secrecy::SecretString;
use serde_json::{Value, json};
use sqlx::PgPool;
use storage::staff::Role;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const OWNER: i64 = 501;
const ADMIN: i64 = 502;
const STRANGER: i64 = 666;
const TOKEN: &str = "123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw";

#[derive(Debug, Clone)]
struct Call {
    method: String,
    body: Value,
    /// id сообщения, которое вернул поддельный Telegram.
    message_id: Option<i64>,
}

impl Call {
    fn chat(&self) -> Option<i64> {
        self.body["chat_id"].as_i64()
    }

    fn text(&self) -> String {
        match self.body["text"].as_str() {
            Some(t) => t.to_owned(),
            // rich-сообщение клиентского бота: смотрим всё тело.
            None => self.body.to_string(),
        }
    }

    /// Данные кнопки, текст которой содержит `label`.
    fn button(&self, label: &str) -> Option<String> {
        self.body["reply_markup"]["inline_keyboard"]
            .as_array()?
            .iter()
            .flat_map(|row| row.as_array().cloned().unwrap_or_default())
            .find(|b| b["text"].as_str().is_some_and(|t| t.contains(label)))
            .and_then(|b| b["callback_data"].as_str().map(str::to_owned))
    }

    /// Подписи кнопок под сообщением.
    fn labels(&self) -> Vec<String> {
        self.body["reply_markup"]["inline_keyboard"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .flat_map(|row| row.as_array().cloned().unwrap_or_default())
                    .filter_map(|b| b["text"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn buttons(&self) -> Vec<String> {
        self.body["reply_markup"]["inline_keyboard"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .flat_map(|row| row.as_array().cloned().unwrap_or_default())
                    .filter_map(|b| b["callback_data"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Default)]
struct State {
    updates: Vec<Value>,
    next_update: i64,
    next_message: i64,
    calls: Vec<Call>,
    /// Сколько следующих `sendMessage` Telegram не примет (500): связь с ним пропала.
    fail_sends: usize,
    /// Следующая новая rich-карточка получит отказ, который допускает HTML-фолбэк.
    reject_next_rich: bool,
    /// Чаты, где бот заблокирован: любой запрос в них — 403. Попытки — в `refused`.
    blocked: std::collections::HashSet<i64>,
    refused: Vec<(String, i64)>,
}

#[derive(Clone)]
struct FakeTelegram {
    base: String,
    state: Arc<Mutex<State>>,
}

impl FakeTelegram {
    async fn start() -> Self {
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
        tokio::spawn(async move {
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
                    let payload = if result.get("__fail").is_some_and(|f| f == 403) {
                        json!({ "ok": false, "error_code": 403, "description": "Forbidden: bot was blocked by the user" })
                            .to_string()
                    } else if result.get("__fail").is_some_and(|f| f == 400) {
                        json!({ "ok": false, "error_code": 400, "description": "Bad Request: rich message rejected" })
                            .to_string()
                    } else if result.get("__fail").is_some() {
                        json!({ "ok": false, "error_code": 500, "description": "Internal Server Error" })
                            .to_string()
                    } else {
                        json!({ "ok": true, "result": result }).to_string()
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    socket.write_all(response.as_bytes()).await.ok();
                    socket.shutdown().await.ok();
                });
            }
        });
        fake
    }

    fn api(&self) -> botapi::BotApi {
        botapi::BotApi::builder(SecretString::from(TOKEN))
            .base_url(&self.base)
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn writes(&self, user: i64, text: &str) {
        self.writes_with(user, text, json!([]));
    }

    fn writes_with(&self, user: i64, text: &str, entities: Value) {
        let mut s = self.lock();
        let id = s.next_update;
        s.next_update += 1;
        s.updates.push(json!({
            "update_id": id,
            "message": {
                "message_id": id,
                "date": 1,
                "chat": { "id": user, "type": "private" },
                "from": { "id": user, "is_bot": false, "first_name": "Staff" },
                "text": text,
                "entities": entities,
            }
        }));
    }

    /// Сотрудник (или кошелёк через инлайн-режим) правит своё сообщение `message_id`.
    fn edits(&self, user: i64, message_id: i64, text: &str) {
        let mut s = self.lock();
        let id = s.next_update;
        s.next_update += 1;
        s.updates.push(json!({
            "update_id": id,
            "edited_message": {
                "message_id": message_id,
                "date": 1,
                "edit_date": 2,
                "chat": { "id": user, "type": "private" },
                "from": { "id": user, "is_bot": false, "first_name": "Staff" },
                "text": text,
            }
        }));
    }

    fn presses(&self, user: i64, message_id: i64, data: &str) {
        let mut s = self.lock();
        let id = s.next_update;
        s.next_update += 1;
        s.updates.push(json!({
            "update_id": id,
            "callback_query": {
                "id": format!("cb{id}"),
                "from": { "id": user, "is_bot": false, "first_name": "Staff" },
                "message": { "message_id": message_id, "date": 1, "chat": { "id": user, "type": "private" } },
                "chat_instance": "1",
                "data": data,
            }
        }));
    }

    fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    fn mark(&self) -> usize {
        self.lock().calls.len()
    }

    /// Первый после `since` экран (новый или правка) в чат `chat`, в тексте которого `needle`.
    async fn screen(&self, since: usize, chat: i64, needle: &str) -> Call {
        for _ in 0..400 {
            if let Some(c) = self.calls().into_iter().skip(since).find(|c| {
                matches!(
                    c.method.as_str(),
                    "sendMessage" | "editMessageText" | "sendRichMessage"
                ) && c.chat() == Some(chat)
                    && c.text().contains(needle)
            }) {
                return c;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        let texts: Vec<String> = self.calls().iter().skip(since).map(Call::text).collect();
        panic!("no screen with {needle:?} for {chat}; got: {texts:#?}");
    }

    async fn respond(&self, method: &str, body: Value) -> Value {
        if method == "getUpdates" {
            let offset = body["offset"].as_i64().unwrap_or(0);
            self.lock()
                .updates
                .retain(|u| u["update_id"].as_i64().unwrap() >= offset);
            for _ in 0..10 {
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
        if let Some(chat) = body["chat_id"].as_i64()
            && s.blocked.contains(&chat)
        {
            s.refused.push((method.to_owned(), chat));
            return json!({ "__fail": 403 });
        }
        if method == "sendMessage" && s.fail_sends > 0 {
            s.fail_sends -= 1;
            return json!({ "__fail": true });
        }
        if method == "sendRichMessage" && s.reject_next_rich {
            s.reject_next_rich = false;
            return json!({ "__fail": 400 });
        }
        let (result, message_id) = match method {
            "getMe" => (
                json!({ "id": 999, "is_bot": true, "first_name": "Exch", "username": "exch_test_bot" }),
                None,
            ),
            "sendMessage" | "sendRichMessage" => {
                s.next_message += 1;
                let id = s.next_message;
                (
                    json!({ "message_id": id, "date": 1, "chat": { "id": body["chat_id"], "type": "private" }, "text": body["text"] }),
                    Some(id),
                )
            }
            "editMessageText" => (
                json!({ "message_id": body["message_id"], "date": 1, "chat": { "id": body["chat_id"], "type": "private" }, "text": body["text"] }),
                body["message_id"].as_i64(),
            ),
            _ => (json!(true), None),
        };
        s.calls.push(Call {
            method: method.to_owned(),
            body,
            message_id,
        });
        result
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

struct Ops {
    pool: PgPool,
    world: userbot::sim::SimWorld,
    ub: userbot::sim::AccountId,
    engine: Arc<Engine>,
    tg: FakeTelegram,
    bot: OpsBot,
    cancel: CancellationToken,
}

impl Drop for Ops {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn ops(pool: PgPool) -> Ops {
    ops_with(pool, |_| {}).await
}

async fn ops_with(pool: PgPool, tune: impl FnOnce(&mut OpsConfig)) -> Ops {
    let e = common::env(pool).await;
    let common::Env {
        pool,
        world,
        ub,
        engine,
        ..
    } = e;
    let engine = Arc::new(engine);
    {
        let mut conn = pool.acquire().await.unwrap();
        storage::staff::ensure_owner(&mut conn, OWNER, "Игнат")
            .await
            .unwrap();
        storage::staff::add(&mut conn, ADMIN, Role::Admin, "Анна", OWNER)
            .await
            .unwrap();
    }
    let tg = FakeTelegram::start().await;
    let bot = OpsBot::new(tg.api(), engine.clone(), pool.clone(), {
        let mut cfg = OpsConfig {
            alert_every: Duration::from_millis(50),
            ..OpsConfig::default()
        };
        tune(&mut cfg);
        cfg
    })
    .unwrap();
    // Один бот: клиентский бот отдаёт обновления пульту первым.
    let client = tgbot::ClientBot::new(
        tg.api(),
        ui::Ui::builtin(ui::EmojiSet::plain(), ui::UiConfig::default()).unwrap(),
        engine.clone(),
        pool.clone(),
        tgbot::ClientBotConfig {
            poll_timeout_secs: 0,
            rich_messages: false,
            ..tgbot::ClientBotConfig::default()
        },
    )
    .with_hook(Arc::new(bot.clone()));
    let cancel = CancellationToken::new();
    {
        let bot = bot.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { bot.run(cancel).await });
    }
    {
        let cancel = cancel.clone();
        tokio::spawn(async move { client.run(cancel).await });
    }
    Ops {
        pool,
        world,
        ub,
        engine,
        tg,
        bot,
        cancel,
    }
}

impl Ops {
    async fn drain(&self) {
        for _ in 0..20 {
            if self.engine.process_due().await.unwrap() == 0 {
                return;
            }
        }
        panic!("operations keep coming");
    }

    /// Нажать кнопку `label` на экране `screen` и дождаться экрана с `needle`.
    async fn press(&self, user: i64, screen: &Call, label: &str, needle: &str) -> Call {
        let data = screen
            .button(label)
            .unwrap_or_else(|| panic!("no button {label:?} in {:?}", screen.buttons()));
        let mark = self.tg.mark();
        self.tg.presses(user, screen.message_id.unwrap(), &data);
        self.tg.screen(mark, user, needle).await
    }

    /// Нажать строку таблицы с текстом `label`: кнопка в тексте (`<tg-button
    /// type="callback_data">`) приходит боту обычной кнопкой под тем же сообщением.
    async fn follow(&self, user: i64, screen: &Call, label: &str, needle: &str) -> Call {
        let body = screen.text().replace("\\\"", "\"");
        let data = body
            .split("<tg-button type=\"callback_data\" style=\"link\" data=\"")
            .skip(1)
            .find_map(|rest| {
                let (data, tail) = rest.split_once('"')?;
                let text = tail.strip_prefix('>')?.split("</tg-button>").next()?;
                text.contains(label).then(|| data.to_owned())
            })
            .unwrap_or_else(|| panic!("no row {label:?} in {body}"));
        let mark = self.tg.mark();
        self.tg.presses(user, screen.message_id.unwrap(), &data);
        self.tg.screen(mark, user, needle).await
    }

    async fn say(&self, user: i64, text: &str, needle: &str) -> Call {
        let mark = self.tg.mark();
        self.tg.writes(user, text);
        self.tg.screen(mark, user, needle).await
    }

    /// Последняя правка сообщения `message_id` в чате `chat` после `mark`; ждёт до 5 с.
    async fn edit_of(&self, mark: usize, chat: i64, message_id: Option<i64>) -> Call {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let found = self.tg.calls().into_iter().skip(mark).rfind(|c| {
                c.method.starts_with("edit")
                    && c.chat() == Some(chat)
                    && c.body["message_id"].as_i64() == message_id
            });
            if let Some(c) = found {
                return c;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no edit of message {message_id:?} in chat {chat}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn audit(&self, action: &str) -> Vec<storage::audit::AuditRow> {
        let mut conn = self.pool.acquire().await.unwrap();
        storage::audit::recent(&mut conn, 50, None, None)
            .await
            .unwrap()
            .into_iter()
            .filter(|a| a.action == action)
            .collect()
    }
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn one_bot_staff_see_the_panel_clients_do_not(pool: PgPool) {
    let o = ops(pool).await;
    // Владелец открывает бота как обычно: у него в меню есть вход в админ-панель.
    let welcome = o.say(OWNER, "/start", "Обмен CryptoBot").await;
    assert!(
        welcome.body.to_string().contains("🛠 Админ-панель"),
        "{}",
        welcome.body
    );
    // Кнопка меню открывает пульт: главный экран, оттуда — меню (макет «вариант А»).
    let dash = o.say(OWNER, "🛠 Админ-панель", "Всё спокойно").await;
    let panel = o.press(OWNER, &dash, "Меню", "<b>Пульт</b>").await;
    assert!(panel.text().contains("вы владелец"), "{}", panel.text());
    assert!(panel.button("Персонал").is_some(), "владельцу — персонал");
    assert!(panel.buttons().iter().all(|b| b.starts_with("a:")));
    let admin = o.say(ADMIN, "/menu", "<b>Пульт</b>").await;
    assert!(admin.text().contains("вы админ"), "{}", admin.text());
    assert!(admin.button("Персонал").is_none());
    // Клиент: обычное приветствие без кнопки, на /admin пульт не отвечает.
    let client = o.say(STRANGER, "/start", "Обмен CryptoBot").await;
    assert!(!client.body.to_string().contains("Админ-панель"));
    let mark = o.tg.mark();
    o.tg.writes(STRANGER, "/admin");
    o.tg.writes(STRANGER, "/start");
    o.tg.screen(mark, STRANGER, "Обмен CryptoBot").await;
    assert!(
        o.tg.calls()
            .iter()
            .skip(mark)
            .all(|c| !c.text().contains("<b>Пульт</b>") && !c.text().contains("Всё спокойно")),
        "постороннему пульт не виден"
    );
    // Кнопка пульта от постороннего (подделка) — ничего не делает.
    let mark = o.tg.mark();
    o.tg.presses(STRANGER, 1, "a:dash");
    o.tg.writes(STRANGER, "/start");
    o.tg.screen(mark, STRANGER, "Обмен CryptoBot").await;
    assert!(
        o.tg.calls()
            .iter()
            .skip(mark)
            .all(|c| !c.text().contains("Всё спокойно"))
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_queue_order_opens_from_the_html_fallback(pool: PgPool) {
    let o = ops(pool).await;
    let (order, _, _) = review_cards(&o).await;
    o.tg.lock().reject_next_rich = true;
    let queue = o.say(OWNER, "/queue", &order.public_id).await;
    assert_eq!(queue.method, "sendMessage");
    let payload = format!("a__o__{}", order.id);
    let url = format!("https://t.me/exch_test_bot?start={payload}");
    assert!(
        queue
            .text()
            .contains(&format!("<a href=\"{url}\">{}</a>", order.public_id)),
        "{}",
        queue.text()
    );
    let opened = o
        .say(OWNER, &format!("/start {payload}"), "Ручной разбор")
        .await;
    assert_eq!(opened.message_id, queue.message_id);
    assert!(opened.text().contains(&order.public_id));
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn staff_payout_limit_needs_owner_confirmation_and_refuses_a_stale_change(pool: PgPool) {
    let o = ops(pool).await;
    let denied = o.say(ADMIN, "/staff", "Это действие доступно").await;
    assert!(!denied.text().contains("Анна"));
    let staff = o.say(OWNER, "/staff", "Потолок").await;
    assert!(staff.text().contains("Анна") && staff.text().contains("300"));
    let select = o
        .press(OWNER, &staff, "Потолок", "Выберите сотрудника")
        .await;
    assert!(select.button("Игнат").is_none());
    let prompt = o.press(OWNER, &select, "Анна", "Сейчас").await;
    assert!(prompt.text().contains("300 USDT"));

    let preview = o.say(OWNER, "125.5", "125.5 USDT").await;
    {
        let mut conn = o.pool.acquire().await.unwrap();
        let staff = storage::staff::get(&mut conn, ADMIN)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(staff.max_manual_payout, None);
    }
    let saved = o.press(OWNER, &preview, "Подтвердить", "Сохранено").await;
    assert!(saved.text().contains("125.5"));
    let audit = o.audit("staff.limit").await;
    assert_eq!(
        audit[0].before,
        Some(json!({"limit":"300","personal": false}))
    );
    assert_eq!(
        audit[0].after,
        Some(json!({"limit":"125.5","personal": true}))
    );

    let select = o
        .press(OWNER, &saved, "Потолок", "Выберите сотрудника")
        .await;
    o.press(OWNER, &select, "Анна", "Сейчас").await;
    let preview = o.say(OWNER, "-", "300 USDT").await;
    {
        let mut conn = o.pool.acquire().await.unwrap();
        assert!(
            storage::staff::set_manual_payout(
                &mut conn,
                ADMIN,
                Role::Admin,
                Some(dec!(125.5)),
                Some(dec!(175))
            )
            .await
            .unwrap()
        );
    }
    let stale = o
        .press(OWNER, &preview, "Подтвердить", "Кнопка устарела")
        .await;
    assert!(stale.text().contains("175"));
    assert_eq!(o.audit("staff.limit").await.len(), 1);

    let select = o
        .press(OWNER, &stale, "Потолок", "Выберите сотрудника")
        .await;
    o.press(OWNER, &select, "Анна", "Сейчас").await;
    let preview = o.say(OWNER, "-", "300 USDT").await;
    o.press(OWNER, &preview, "Подтвердить", "Сохранено").await;
    let mut conn = o.pool.acquire().await.unwrap();
    let staff = storage::staff::get(&mut conn, ADMIN)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(staff.max_manual_payout, None);
    assert_eq!(staff.manual_payout_limit(), Some(dec!(300)));
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn personnel_table_can_change_a_role_and_disable_the_member(pool: PgPool) {
    let o = ops(pool).await;
    let staff = o.say(OWNER, "/staff", "Потолок").await;
    let profile = o
        .follow(OWNER, &staff, "Вы", "Потолок ручной выплаты")
        .await;
    assert!(profile.text().contains(&format!("<code>{OWNER}</code>")));
    assert!(profile.button("Отключить").is_none());
    let staff = o.press(OWNER, &profile, "Персонал", "Потолок").await;
    let select = o.press(OWNER, &staff, "Роль", "Выберите сотрудника").await;
    assert!(select.button("Игнат").is_none());
    let role = o.press(OWNER, &select, "Анна", "Оператор").await;
    let saved = o.press(OWNER, &role, "Подтвердить", "Сохранено").await;
    assert_eq!(o.audit("staff.role").await.len(), 1);
    let select = o
        .press(OWNER, &saved, "Отключить", "Выберите сотрудника")
        .await;
    let disable = o
        .press(OWNER, &select, "Анна", "перестанет ему отвечать")
        .await;
    let staff = o.press(OWNER, &disable, "Подтвердить", "Сохранено").await;
    let profile = o.follow(OWNER, &staff, "Анна", "Доступ отключён").await;
    assert!(profile.text().contains(&format!("<code>{ADMIN}</code>")));
    assert!(profile.button("Потолок").is_none());
    let mut conn = o.pool.acquire().await.unwrap();
    let member = storage::staff::get(&mut conn, ADMIN)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(member.role, Role::Operator);
    assert!(!member.active());
    drop(conn);
    assert_eq!(o.audit("staff.disable").await.len(), 1);
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn fee_change_needs_a_preview_and_a_single_use_confirmation(pool: PgPool) {
    let o = ops(pool).await;
    let panel = o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    let dirs = o.press(OWNER, &panel, "Направления", "Рычаг k").await;
    let card = o.press(OWNER, &dirs, "CB → xR", "Рычаг резерва").await;
    let ask = o.press(OWNER, &card, "Комиссия", "Новая комиссия").await;
    assert!(ask.text().contains("2.5"));
    // Неверный ввод — вопрос ещё раз, ничего не меняется.
    o.say(OWNER, "две целых", "Нужно число").await;
    let preview = o.say(OWNER, "3", "проверьте изменение").await;
    assert!(preview.text().contains("2.5") && preview.text().contains("3\u{a0}%"));
    let confirm = preview.button("Подтвердить").unwrap();
    // Чужое подтверждение не срабатывает.
    let mark = o.tg.mark();
    o.tg.presses(ADMIN, preview.message_id.unwrap(), &confirm);
    o.tg.screen(mark, ADMIN, "устарела").await;
    let done = o.press(OWNER, &preview, "Подтвердить", "Сохранено").await;
    assert!(done.text().contains("3\u{a0}%"));
    // Повторное нажатие той же кнопки — уже нет.
    let mark = o.tg.mark();
    o.tg.presses(OWNER, preview.message_id.unwrap(), &confirm);
    o.tg.screen(mark, OWNER, "устарела").await;

    let mut conn = o.pool.acquire().await.unwrap();
    let dir = storage::directions::get(&mut conn, Direction::CbToXrCheck)
        .await
        .unwrap();
    assert_eq!(dir.fee_pct, dec!(3));
    // Полоса рычага сдвинулась вместе с комиссией.
    assert_eq!((dir.fee_floor_pct, dir.fee_cap_pct), (dec!(2.0), dec!(4.0)));
    drop(conn);
    let audit = o.audit("direction.set").await;
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].staff_id, OWNER);
    assert!(audit[0].double_confirmed);
    assert_eq!(audit[0].after.as_ref().unwrap()["fee_pct"], "3");
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn admin_cannot_move_the_fee_by_more_than_one_point(pool: PgPool) {
    let o = ops(pool).await;
    let panel = o.say(ADMIN, "/dir", "Рычаг k").await;
    let card = o.press(ADMIN, &panel, "xR → CB", "Рычаг резерва").await;
    let ask = o.press(ADMIN, &card, "Комиссия", "Новая комиссия").await;
    let _ = ask;
    let preview = o.say(ADMIN, "4", "проверьте изменение").await;
    assert!(preview.text().contains("только владелец"));
    assert!(preview.button("Подтвердить").is_none());
    // «Отправить владельцу» (шаг 3): владелец получает тот же предпросмотр с пометкой, кто
    // предлагает, и решает сам.
    let mark = o.tg.mark();
    o.press(ADMIN, &preview, "Отправить владельцу", "Отправил владельцу")
        .await;
    let request = o.tg.screen(mark, OWNER, "Анна предлагает").await;
    assert!(
        request.text().contains("проверьте изменение"),
        "{}",
        request.text()
    );
    o.press(OWNER, &request, "Подтвердить", "Сохранено").await;
    let mut conn = o.pool.acquire().await.unwrap();
    let row = storage::directions::get(&mut conn, Direction::XrToCbCheck)
        .await
        .unwrap();
    assert_eq!(row.fee_pct, dec!(4), "владелец применил");
    drop(conn);
    // Небольшое изменение админ делает сам.
    let card = o.press(ADMIN, &panel, "xR → CB", "Рычаг резерва").await;
    o.press(ADMIN, &card, "Комиссия", "Новая комиссия").await;
    // Владелец поставил 4 %: 3.5 % — сдвиг на 0.5 п.п.
    let preview = o.say(ADMIN, "3.5", "проверьте изменение").await;
    o.press(ADMIN, &preview, "Подтвердить", "Сохранено").await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn shared_lever_keeps_direction_bands_and_expires_old_edits(pool: PgPool) {
    let o = ops(pool).await;
    // Старая база могла хранить разные k. Пульт показывает это, не меняя цены при открытии.
    sqlx::query("UPDATE directions SET skew_k_pct = 0.3 WHERE code = 'xr_to_cb_check'")
        .execute(&o.pool)
        .await
        .unwrap();
    let before = storage::directions::all(&mut o.pool.acquire().await.unwrap())
        .await
        .unwrap();
    o.tg.lock().reject_next_rich = true;
    let dirs = o.say(ADMIN, "/dir", "Рычаг k = разный").await;
    assert_eq!(dirs.method, "sendMessage");
    let card = o.press(ADMIN, &dirs, "CB → xR", "Рычаг резерва").await;
    o.press(ADMIN, &card, "Пол и потолок", "через пробел").await;
    o.say(ADMIN, "0.1 1.4 3.6", "два числа").await;
    let old_band = o.say(ADMIN, "1.4 3.6", "проверьте изменение").await;

    let dirs = o.say(OWNER, "/dir", "Рычаг k = разный").await;
    o.press(OWNER, &dirs, "Рычаг", "Общий коэффициент").await;
    o.say(OWNER, "1000", "до 6 знаков").await;
    let preview = o.say(OWNER, "0,6", "Новые заявки — сразу").await;
    assert!(preview.text().contains("<code>0.3</code> → <b>0.6</b>"));
    let done = o.press(OWNER, &preview, "Подтвердить", "Сохранено").await;
    assert!(done.text().contains("Рычаг k = 0.6"), "{}", done.text());
    o.press(ADMIN, &old_band, "Подтвердить", "устарела").await;
    let mut conn = o.pool.acquire().await.unwrap();
    let after = storage::directions::all(&mut conn).await.unwrap();
    for (mut expected, row) in before.into_iter().zip(after) {
        expected.skew_k_pct = dec!(0.6);
        assert_eq!(row, expected);
    }
    drop(conn);
    let audit = o.audit("direction.lever").await;
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].after.as_ref().unwrap()["skew_k_pct"], "0.6");
    assert!(o.audit("direction.set").await.is_empty());

    let dirs = o.say(ADMIN, "/dir", "Рычаг k = 0.6").await;
    o.press(ADMIN, &dirs, "Рычаг", "Общий коэффициент").await;
    let major = o.say(ADMIN, "2", "подтверждает владелец").await;
    assert!(major.button("Подтвердить").is_none());
    assert!(major.button("Отправить владельцу").is_some());
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn tiers_and_switch_to_flat_and_back(pool: PgPool) {
    let o = ops(pool).await;
    let panel = o.say(OWNER, "/dir", "Рычаг k").await;
    let card = o.press(OWNER, &panel, "CB → xR", "Рычаг резерва").await;
    // Режим ступеней без ступеней — нельзя.
    o.press(
        OWNER,
        &card,
        "Перейти на ступени",
        "Сначала задайте ступени",
    )
    .await;
    let card = o.press(OWNER, &panel, "CB → xR", "Рычаг резерва").await;
    o.press(OWNER, &card, "Ступени", "Ступени: строка").await;
    o.say(OWNER, "10 2.5", "Не понял формат").await;
    let preview = o
        .say(OWNER, "0 2.5\n100 1.8\n250 1.2", "проверьте изменение")
        .await;
    let card = o.press(OWNER, &preview, "Подтвердить", "Сохранено").await;
    assert!(card.text().contains("сохранены, но сейчас не действуют"));
    let preview = o
        .press(OWNER, &card, "Перейти на ступени", "проверьте изменение")
        .await;
    let card = o.press(OWNER, &preview, "Подтвердить", "Сохранено").await;
    assert!(card.text().contains("Комиссия: <b>ступени</b>"));
    let mut conn = o.pool.acquire().await.unwrap();
    let dir = storage::directions::get(&mut conn, Direction::CbToXrCheck)
        .await
        .unwrap();
    assert_eq!(dir.fee_mode, domain::fees::FeeMode::Tiers);
    assert_eq!(dir.fee_tiers.unwrap().tiers().len(), 3);
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn settings_toggle_the_newcomer_limit_and_the_kill_switch(pool: PgPool) {
    let o = ops(pool).await;
    let settings = o.say(OWNER, "/settings", "Настройки").await;
    let preview = o
        .press(OWNER, &settings, "Лимит новичка", "Лимит нового клиента")
        .await;
    let settings = o.press(OWNER, &preview, "Подтвердить", "Сохранено").await;
    let mut conn = o.pool.acquire().await.unwrap();
    assert!(
        !storage::settings::flag(&mut conn, storage::user_terms::NEWCOMER_LIMIT, true)
            .await
            .unwrap()
    );
    // Стоп-кран включает любой, выключает только владелец.
    let preview = o
        .press(
            OWNER,
            &settings,
            "Включить стоп-кран",
            "Остановить все выплаты?",
        )
        .await;
    // П-13: словами — сколько выплат встанет и что будет с новыми заявками.
    assert!(
        preview.text().contains("Выплат в очереди нет")
            && preview.text().contains("в разбор")
            && !preview.text().contains("random_id"),
        "{}",
        preview.text()
    );
    let mark = o.tg.mark();
    o.press(OWNER, &preview, "Остановить", "Сохранено").await;
    assert!(
        storage::settings::flag(&mut conn, "kill_switch_payouts", false)
            .await
            .unwrap()
    );
    // Шаг 2.8: стоп-кран — критичная тревога всему персоналу («Выплаты остановлены», кто и когда).
    let alert = o.tg.screen(mark, ADMIN, "Выплаты остановлены").await;
    assert!(alert.text().contains("Игнат"), "{}", alert.text());
    assert!(alert.button("Обслуживание").is_some());
    let admin = o.say(ADMIN, "/settings", "Настройки").await;
    o.press(ADMIN, &admin, "Выключить стоп-кран", "роли «Владелец»")
        .await;
    // Снять стоп-кран — ещё и контрольная строка (макет «Стоп-кран», шаг 3): не то слово —
    // ничего не сделано; «подтверждаю» в любом регистре — снят.
    let settings = o.say(OWNER, "/settings", "Настройки").await;
    let preview = o
        .press(OWNER, &settings, "Выключить стоп-кран", "Стоп-кран выплат")
        .await;
    o.press(OWNER, &preview, "Подтвердить", "ПОДТВЕРЖДАЮ").await;
    o.say(OWNER, "да", "Не совпало").await;
    assert!(
        storage::settings::flag(&mut conn, "kill_switch_payouts", false)
            .await
            .unwrap(),
        "не то слово — стоп-кран на месте"
    );
    let settings = o.say(OWNER, "/settings", "Настройки").await;
    let preview = o
        .press(OWNER, &settings, "Выключить стоп-кран", "Стоп-кран выплат")
        .await;
    o.press(OWNER, &preview, "Подтвердить", "ПОДТВЕРЖДАЮ").await;
    o.say(OWNER, " подтверждаю ", "Сохранено").await;
    assert!(
        !storage::settings::flag(&mut conn, "kill_switch_payouts", false)
            .await
            .unwrap()
    );
    // Стоп-кран снят — тревога закрыта сама.
    let open = storage::alerts::open_by_key(&mut conn, "kill_switch")
        .await
        .unwrap();
    assert!(open.is_none(), "{open:?}");
}

/// П-13 (аудит 16.11): тревога «Стоп-кран» говорит «Выключит владелец: введите ПОДТВЕРЖДАЮ» —
/// пульт ждёт это слово от владельца без кнопок. Админу то же слово — отказ; без стоп-крана
/// слово остаётся пульту, а не уходит клиентскому боту («Пришлите чек»).
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn the_owner_lifts_the_kill_switch_by_typing_the_word(pool: PgPool) {
    let o = ops(pool).await;
    let calm = o.say(OWNER, "ПОДТВЕРЖДАЮ", "Подтверждать нечего").await;
    assert!(calm.text().contains("<b>Пульт</b>"), "{}", calm.text());
    let settings = o.say(ADMIN, "/settings", "Настройки").await;
    let preview = o
        .press(
            ADMIN,
            &settings,
            "Включить стоп-кран",
            "Остановить все выплаты?",
        )
        .await;
    let mark = o.tg.mark();
    o.press(ADMIN, &preview, "Остановить", "Сохранено").await;
    let alert = o.tg.screen(mark, OWNER, "Выплаты остановлены").await;
    assert!(alert.text().contains("ПОДТВЕРЖДАЮ"), "{}", alert.text());
    let kill_on = || async {
        let mut conn = o.pool.acquire().await.unwrap();
        storage::settings::flag(&mut conn, "kill_switch_payouts", false)
            .await
            .unwrap()
    };
    o.say(ADMIN, "ПОДТВЕРЖДАЮ", "роли «Владелец»").await;
    assert!(kill_on().await, "админ стоп-кран не снимает");
    o.say(OWNER, " подтверждаю ", "Сохранено").await;
    assert!(!kill_on().await, "владелец снял словом");
    // П-10: тревога у всех — серая, кто снял.
    o.bot.deliver_alerts().await.unwrap();
    let done = o.edit_of(0, OWNER, alert.message_id).await;
    assert!(
        done.text().contains("Решено: Игнат") && done.text().contains("Стоп-кран выплат: выкл"),
        "{}",
        done.text()
    );
    let mut conn = o.pool.acquire().await.unwrap();
    assert!(
        storage::alerts::open_by_key(&mut conn, "kill_switch")
            .await
            .unwrap()
            .is_none()
    );
    drop(conn);
    let flags = o.audit("settings.flag").await;
    assert_eq!(flags.len(), 2, "включил админ, снял владелец");
    assert!(flags.iter().all(|a| a.double_confirmed));
}

/// П-13: в тревоге «Стоп-кран» — «Обслуживание 30 мин» (макет), дальше экран «Обслуживание · до
/// 15:30» с «+15 мин» и «Закончить». Срок вышел — обслуживание кончилось само.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn maintenance_for_30_minutes_from_the_kill_switch_alert(pool: PgPool) {
    let o = ops(pool).await;
    let settings = o.say(ADMIN, "/settings", "Настройки").await;
    let preview = o
        .press(
            ADMIN,
            &settings,
            "Включить стоп-кран",
            "Остановить все выплаты?",
        )
        .await;
    let mark = o.tg.mark();
    o.press(ADMIN, &preview, "Остановить", "Сохранено").await;
    let alert = o.tg.screen(mark, ADMIN, "Выплаты остановлены").await;
    let maint = |p: &PgPool| {
        let p = p.clone();
        async move {
            let mut conn = p.acquire().await.unwrap();
            (
                storage::settings::flag_on(&mut conn, "maintenance")
                    .await
                    .unwrap(),
                storage::settings::on_until(&mut conn, "maintenance")
                    .await
                    .unwrap(),
            )
        }
    };
    let preview = o
        .press(ADMIN, &alert, "Обслуживание 30 мин", "Режим обслуживания")
        .await;
    let card = o
        .press(ADMIN, &preview, "Подтвердить", "Обслуживание · до")
        .await;
    assert!(
        card.text().contains("Новые чеки не принимаю"),
        "{}",
        card.text()
    );
    let (on, until) = maint(&o.pool).await;
    let left = until.expect("срок") - chrono::Utc::now();
    assert!(on && left > chrono::Duration::minutes(29) && left <= chrono::Duration::minutes(30));

    let preview = o.press(ADMIN, &card, "+15 мин", "Режим обслуживания").await;
    let card = o
        .press(ADMIN, &preview, "Подтвердить", "Обслуживание · до")
        .await;
    let left = maint(&o.pool).await.1.expect("срок") - chrono::Utc::now();
    assert!(left > chrono::Duration::minutes(44), "+15 к сроку: {left}");

    // «Текст для клиентов» (решение владельца 18.11, вариант «а»): строка под «Обновляемся до…».
    let ask = o
        .press(ADMIN, &card, "Текст для клиентов", "Текст для клиентов")
        .await;
    assert!(ask.button("Отмена").is_some());
    let preview = o
        .say(ADMIN, "Меняем сервер — вернёмся вовремя", "Меняем сервер")
        .await;
    let card = o
        .press(ADMIN, &preview, "Подтвердить", "Обслуживание · до")
        .await;
    {
        let mut conn = o.pool.acquire().await.unwrap();
        assert_eq!(
            storage::settings::on_note(&mut conn, "maintenance")
                .await
                .unwrap()
                .as_deref(),
            Some("Меняем сервер — вернёмся вовремя")
        );
    }
    assert_eq!(o.audit("settings.maint_text").await.len(), 1);
    let preview = o
        .press(ADMIN, &card, "Закончить", "Режим обслуживания")
        .await;
    o.press(ADMIN, &preview, "Подтвердить", "Сохранено").await;
    assert_eq!(maint(&o.pool).await, (false, None));
    assert_eq!(o.audit("settings.flag").await.len(), 4);
    {
        // Закончили — текст для клиентов снят: в следующий раз не всплывёт старый.
        let mut conn = o.pool.acquire().await.unwrap();
        let v = storage::settings::get(&mut conn, "maintenance")
            .await
            .unwrap()
            .unwrap();
        assert!(v.get("text").is_none(), "{v}");
    }

    // Срок вышел — обслуживание выключено для всех, хотя запись ещё «вкл».
    let mut conn = o.pool.acquire().await.unwrap();
    let past = chrono::Utc::now() - chrono::Duration::seconds(1);
    storage::settings::set(
        &mut conn,
        "maintenance",
        &serde_json::json!({ "on": true, "until": past.to_rfc3339() }),
        None,
    )
    .await
    .unwrap();
    drop(conn);
    assert_eq!(maint(&o.pool).await, (false, None));
    let settings = o.say(ADMIN, "/settings", "Настройки").await;
    let preview = o
        .press(ADMIN, &settings, "Обслуживание", "Режим обслуживания")
        .await;
    assert!(
        preview.text().contains("выкл → <b>вкл</b>"),
        "{}",
        preview.text()
    );
}

/// Шаг 2.8: движок не проходит круг дольше `engine_down_after` — тревога «Движок не работает»
/// с последней ошибкой; ведёт в «Систему».
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_silent_engine_raises_the_engine_down_alert(pool: PgPool) {
    let o = ops_with(pool, |c| c.engine_down_after = Duration::from_millis(300)).await;
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    // В тестах цикл движка не запущен — он «молчит» с запуска пульта.
    let alert = o.tg.screen(0, OWNER, "Движок не работает").await;
    assert!(alert.button("Система").is_some());
    assert!(alert.button("Принял").is_some());
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn promo_hint_arrives_and_one_button_switches_the_promo_on(pool: PgPool) {
    let o = ops(pool).await;
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    {
        let mut conn = o.pool.acquire().await.unwrap();
        for (platform, target) in [(CB, dec!(400)), (XR, dec!(2000))] {
            let (wallet, _) = storage::wallets::active(&mut conn, platform)
                .await
                .unwrap()
                .unwrap();
            storage::admin::set_thresholds(
                &mut conn,
                wallet,
                Asset::Usdt,
                storage::admin::Thresholds::from_target(target),
            )
            .await
            .unwrap();
        }
    }
    let mark = o.tg.mark();
    o.engine.promo_tick().await.unwrap();
    let hint = o.tg.screen(mark, OWNER, "Перекос резерва").await;
    assert!(hint.text().contains("xRocket → CryptoBot"));
    let preview = o
        .press(OWNER, &hint, "Включить", "Включить выгодный курс")
        .await;
    let promo = o.press(OWNER, &preview, "Подтвердить", "Сохранено").await;
    // Карточка «Ребалансировка» (шаг 2): кто включил — в строке выгодного курса.
    assert!(
        promo.text().contains("включили вручную"),
        "{}",
        promo.text()
    );
    let mut conn = o.pool.acquire().await.unwrap();
    let states = engine::fees::promo_states(&mut conn).await.unwrap();
    assert!(states["xr_to_cb_check"].on);
    // Подсказка закрыта: перекос решён включением.
    assert!(
        storage::alerts::open(&mut conn, 50)
            .await
            .unwrap()
            .iter()
            .all(|a| a.dedup_key != "promo_hint:xr_to_cb_check")
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn manual_review_alert_leads_to_a_refund_decision(pool: PgPool) {
    let o = ops(pool).await;
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    // Выплата не удалась: в кошельке xRocket 5 USDT вместо 1000.
    o.world.set_balance(o.ub, XR, Asset::Usdt, dec!(5)).unwrap();
    let url = o.world.client_check(CB, usdt(dec!(100))).unwrap();
    let link = parsers::links::parse_url(&url).unwrap();
    let order = accepted(o.engine.submit_check(common::CLIENT, &link).await.unwrap());
    let mark = o.tg.mark();
    o.drain().await;
    let alert = o.tg.screen(mark, OWNER, "Нужен разбор").await;
    // Макет «Алерт и „Принял“» (П-6, П-7) в виде «Полировки» 16.11: публичный номер, суммы с
    // кошельками, клиент — моно; без «№15».
    let text = alert.text();
    assert!(text.contains(&order.public_id), "{text}");
    assert!(!text.contains(&format!("№{}", order.id)), "{text}");
    assert!(
        text.contains("<code>100.00</code> CryptoBot → ")
            && text.contains(" xRocket · клиент <code>111</code>"),
        "{text}"
    );
    assert!(alert.button("Взять").is_some() && alert.button("Принял").is_some());
    // Админ тоже получил событие.
    o.tg.screen(mark, ADMIN, "Нужен разбор").await;
    o.world
        .set_balance(o.ub, XR, Asset::Usdt, dec!(1000))
        .unwrap();
    let card = o.press(OWNER, &alert, "Открыть", "Ручной разбор").await;
    // Карточка по макету: деньги клиента проведены — «Принято» без «не проведено».
    assert!(card.text().contains("Принято"), "{}", card.text());
    assert!(!card.text().contains("не проведено"), "{}", card.text());
    assert!(card.text().contains("Ход · "), "{}", card.text());
    let ask = o.press(OWNER, &card, "Вернуть", "Коротко причина").await;
    assert!(ask.text().contains(&order.public_id), "{}", ask.text());
    o.say(OWNER, "ок", "короче 3 символов").await;
    // Макет «Подтверждение» (шаг 3): вопрос, публичный номер, что произойдёт, «Необратимо»,
    // 60 с; кнопка — красная, с действием и суммой.
    let preview = o
        .say(OWNER, "клиент просит вернуть", "Вернуть клиенту?")
        .await;
    let text = preview.text();
    assert!(text.contains(&order.public_id), "{text}");
    assert!(
        text.contains("Необратимо") && text.contains("60 с"),
        "{text}"
    );
    let confirm = preview.body["reply_markup"]["inline_keyboard"][0][0].clone();
    assert!(
        confirm["text"].as_str().unwrap().contains("Вернуть"),
        "{confirm}"
    );
    assert_eq!(confirm["style"], "danger", "{confirm}");
    let done = o.press(OWNER, &preview, "Вернуть", "<b>Возврат</b>").await;
    // П-7: итог — с публичным номером, без внутреннего «№15» (отдельным сообщением, концепция Б
    // пульта 15.11).
    assert!(
        done.text()
            .contains(&format!("<code>{}</code>", order.public_id)),
        "{}",
        done.text()
    );
    assert!(!done.text().contains(&format!("№{}", order.id)));
    o.drain().await;
    let mut conn = o.pool.acquire().await.unwrap();
    let after = storage::orders::get(&mut conn, order.id, false)
        .await
        .unwrap();
    assert_eq!(after.state, OrderState::Refunded);
    let audit = storage::audit::recent(&mut conn, 10, None, Some(("order", &order.id.to_string())))
        .await
        .unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "order.refund" && a.staff_id == OWNER)
    );
    drop(conn);
    // Заявка ушла с разбора — событие закрылось само.
    o.bot.deliver_alerts().await.unwrap();
    let mut conn = o.pool.acquire().await.unwrap();
    assert!(
        storage::alerts::open(&mut conn, 50)
            .await
            .unwrap()
            .iter()
            .all(|a| a.title != "manual_review")
    );
    drop(conn);
    // П-10 (картинка принята 18.11): решённое — правкой у всех: серое, кто и что, без кнопок.
    // Копию мог поправить и фоновый круг тревог — смотрим все правки с начала.
    let done = o.edit_of(0, OWNER, alert.message_id).await;
    let text = done.text();
    assert!(
        text.contains("Решено: Игнат") && text.contains("вернул(а) деньги"),
        "{text}"
    );
    assert!(
        text.contains(&order.public_id) && !text.contains("🚨"),
        "{text}"
    );
    assert!(done.buttons().is_empty(), "{:?}", done.buttons());
    let mark2 = o.tg.mark();
    o.bot.deliver_alerts().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let again =
        o.tg.calls()
            .into_iter()
            .skip(mark2)
            .filter(|c| {
                c.method.starts_with("edit")
                    && c.chat() == Some(OWNER)
                    && c.body["message_id"].as_i64() == alert.message_id
            })
            .count();
    assert_eq!(again, 0, "решённую копию правят один раз");
}

/// П-5 (аудит 16.11): кнопка в тревоге открывает экран новым сообщением, тревога остаётся
/// тревогой. «Принял» коллеги правит копии тревоги, а карточку, в которой работают, не трогает.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_button_in_an_alert_opens_a_new_message_and_the_alert_stays(pool: PgPool) {
    let o = ops(pool).await;
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    o.say(ADMIN, "/menu", "<b>Пульт</b>").await;
    o.world.set_balance(o.ub, XR, Asset::Usdt, dec!(5)).unwrap();
    let url = o.world.client_check(CB, usdt(dec!(100))).unwrap();
    let link = parsers::links::parse_url(&url).unwrap();
    accepted(o.engine.submit_check(common::CLIENT, &link).await.unwrap());
    let mark = o.tg.mark();
    o.drain().await;
    let alert = o.tg.screen(mark, OWNER, "Нужен разбор").await;
    let admin_alert = o.tg.screen(mark, ADMIN, "Нужен разбор").await;
    let card = o.press(OWNER, &alert, "Открыть", "Ручной разбор").await;
    assert!(card.method.starts_with("send"), "{}", card.method);
    assert_ne!(card.message_id, alert.message_id);
    let mark = o.tg.mark();
    o.press(ADMIN, &admin_alert, "Принял", "Принял").await;
    // Правка копии владельца идёт после ответа админу — ждём её.
    o.edit_of(mark, OWNER, alert.message_id).await;
    let touched: Vec<i64> =
        o.tg.calls()
            .into_iter()
            .skip(mark)
            .filter(|c| c.method.starts_with("edit") && c.chat() == Some(OWNER))
            .filter_map(|c| c.body["message_id"].as_i64())
            .collect();
    assert!(
        !touched.contains(&card.message_id.unwrap()),
        "карточку не тронули: {touched:?}"
    );
    // Макет «Алерт и „Принял“»: «⚪ Разбор · K7M2Q9XR4T / Принял Анна · 14:07 — повторы
    // остановлены» и одна «Открыть».
    let acked = o.edit_of(mark, OWNER, alert.message_id).await;
    let text = acked.text();
    assert!(
        text.contains("<b>Разбор</b>") && text.contains("Принял Анна") && !text.contains("🚨"),
        "{text}"
    );
    assert_eq!(acked.buttons().len(), 1, "{:?}", acked.buttons());
    assert!(acked.button("Открыть").is_some());
}

/// П-10 (картинка принята 18.11): звук по роли. Недостача оператору не приходит (решить её он
/// не может), админу и владельцу — со звуком; «Перекос резерва» — им же, но тихо; разбор —
/// всем со звуком.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn alerts_reach_roles_that_can_act_with_sound_by_role(pool: PgPool) {
    let o = ops(pool).await;
    // Метка — до того, как тревоги подняты: бот в фоне сам рассылает тревоги и может успеть
    // раньше `deliver_alerts` ниже (тест падал под нагрузкой).
    let mark = o.tg.mark();
    {
        let mut conn = o.pool.acquire().await.unwrap();
        storage::staff::add(&mut conn, OPERATOR, Role::Operator, "Олег", OWNER)
            .await
            .unwrap();
        for (key, title, level) in [
            (
                "balance_mismatch:xrocket",
                "balance_mismatch",
                storage::alerts::Level::Critical,
            ),
            (
                "promo_hint:xr_to_cb_check",
                "promo_hint",
                storage::alerts::Level::Warn,
            ),
        ] {
            storage::alerts::raise(
                &mut conn,
                &storage::alerts::NewAlert {
                    level,
                    topic: storage::alerts::Topic::Errors,
                    dedup_key: key,
                    title,
                    payload: serde_json::json!({"platform": "xrocket", "diff": "-12",
                        "expected": "640", "actual": "628", "direction": "xr_to_cb_check"}),
                },
            )
            .await
            .unwrap();
        }
    }
    o.bot.deliver_alerts().await.unwrap();
    let sent = |chat: i64, needle: &str| {
        o.tg.calls()
            .into_iter()
            .skip(mark)
            .filter(|c| c.method == "sendMessage" && c.chat() == Some(chat))
            .find(|c| c.text().contains(needle))
    };
    let short = sent(ADMIN, "меньше на").expect("недостача админу");
    assert_eq!(short.body["disable_notification"], false, "со звуком");
    assert!(sent(OWNER, "меньше на").is_some());
    assert!(
        sent(OPERATOR, "меньше на").is_none(),
        "оператору не приходит"
    );
    let hint = sent(ADMIN, "Перекос резерва").expect("перекос админу");
    assert_eq!(hint.body["disable_notification"], true, "тихо");
    assert!(sent(OPERATOR, "Перекос резерва").is_none());
}

/// Заявка на разборе после неудачной выплаты; владелец и админ открыли её карточку.
async fn review_cards(o: &Ops) -> (storage::orders::OrderRow, Call, Call) {
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    o.say(ADMIN, "/menu", "<b>Пульт</b>").await;
    o.world.set_balance(o.ub, XR, Asset::Usdt, dec!(5)).unwrap();
    let url = o.world.client_check(CB, usdt(dec!(100))).unwrap();
    let link = parsers::links::parse_url(&url).unwrap();
    let order = accepted(o.engine.submit_check(common::CLIENT, &link).await.unwrap());
    let mark = o.tg.mark();
    o.drain().await;
    let owner_alert = o.tg.screen(mark, OWNER, "Нужен разбор").await;
    let admin_alert = o.tg.screen(mark, ADMIN, "Нужен разбор").await;
    o.world
        .set_balance(o.ub, XR, Asset::Usdt, dec!(1000))
        .unwrap();
    let owner = o
        .press(OWNER, &owner_alert, "Открыть", "Ручной разбор")
        .await;
    let admin = o
        .press(ADMIN, &admin_alert, "Открыть", "Ручной разбор")
        .await;
    (order, owner, admin)
}

/// П-12 (аудит 16.11): найденная поиском карточка — с кнопками роли того, кто ищет. Раньше
/// владелец находил заявку без «Закрыть без выплаты», а оператор видел у клиента «Бан» и
/// «Инвестиции», которые ему откажут.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn search_shows_the_searchers_buttons(pool: PgPool) {
    let o = ops(pool).await;
    let (order, _, _) = review_cards(&o).await;
    let found = o
        .say(
            OWNER,
            &format!("/find {}", order.public_id),
            "Ручной разбор",
        )
        .await;
    assert!(
        found.button("Закрыть без выплаты").is_some() && found.button("Выплачено").is_some(),
        "{:?}",
        found.buttons()
    );
    {
        let mut conn = o.pool.acquire().await.unwrap();
        storage::staff::add(&mut conn, OPERATOR, Role::Operator, "Олег", OWNER)
            .await
            .unwrap();
    }
    let client = o
        .say(OPERATOR, &format!("/user {}", common::CLIENT), "Клиент")
        .await;
    let labels: Vec<String> = client.body["reply_markup"]["inline_keyboard"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|r| r.as_array().cloned().unwrap_or_default())
        .filter_map(|b| b["text"].as_str().map(str::to_owned))
        .collect();
    assert!(
        !labels
            .iter()
            .any(|l| l.contains("Бан") || l.contains("Инвестиции")),
        "{labels:?}"
    );
}

async fn order_state(o: &Ops, id: i64) -> OrderState {
    let mut conn = o.pool.acquire().await.unwrap();
    storage::orders::get(&mut conn, id, false)
        .await
        .unwrap()
        .state
}

/// П-2 (аудит 16.11): «Взять» закрепляет заявку. Второй сотрудник не примет противоречивое
/// денежное решение — видит, кто взял, и может только «Забрать себе»; прежний получает
/// предупреждение.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn taking_an_order_locks_its_decisions_to_the_taker(pool: PgPool) {
    let o = ops(pool).await;
    let (order, owner, admin) = review_cards(&o).await;
    o.press(ADMIN, &admin, "Взять", "Заявку взял(а) Анна").await;
    // Владелец нажимает «Взять» и «Вернуть» на своей карточке — заявка уже у Анны.
    let taken = o.press(OWNER, &owner, "Взять", "уже взял(а)").await;
    assert!(taken.text().contains("Анна"), "{}", taken.text());
    let taken = o.press(OWNER, &owner, "Вернуть", "уже взял(а)").await;
    assert_eq!(order_state(&o, order.id).await, OrderState::ManualReview);
    // «Забрать себе»: заявка у владельца, Анне — предупреждение.
    let mark = o.tg.mark();
    let mine = o.press(OWNER, &taken, "Забрать себе", "Заявка ваша").await;
    assert!(mine.text().contains("Анна"), "{}", mine.text());
    let notice = o.tg.screen(mark, ADMIN, "забрал(а) себе").await;
    assert!(
        notice.text().contains(&order.public_id) && notice.text().contains("Игнат"),
        "{}",
        notice.text()
    );
    // Теперь решить не может Анна, а владелец — может.
    o.press(ADMIN, &admin, "Вернуть", "уже взял(а)").await;
    o.press(OWNER, &mine, "Вернуть", "Коротко причина").await;
    let preview = o
        .say(OWNER, "клиент просит вернуть", "Вернуть клиенту?")
        .await;
    o.press(OWNER, &preview, "Вернуть", "<b>Возврат</b>").await;
    o.drain().await;
    assert_eq!(order_state(&o, order.id).await, OrderState::Refunded);
    let takes = o.audit("order.take").await;
    assert_eq!(takes.len(), 2, "{takes:?}");
    // П-31: журнал словами (макет «Журнал»): «взял(а) K7M2…», свои действия — «Вы», без кодов.
    let journal = o.say(OWNER, "/audit", "Журнал").await;
    let text = journal.text();
    assert!(
        text.contains("взял(а)") && text.contains(&order.public_id) && text.contains("Вы"),
        "{text}"
    );
    assert!(text.contains("вернул(а) деньги"), "{text}");
    assert!(
        !text.contains("order.take") && !text.contains("order.refund"),
        "{text}"
    );
}

/// Решение, начатое до того, как заявку забрали, не выполняется: подтверждение перепроверяет,
/// кто держит заявку.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_decision_confirmed_after_a_take_over_is_refused(pool: PgPool) {
    let o = ops(pool).await;
    let (order, owner, admin) = review_cards(&o).await;
    // Анна начинает возврат: никто не брал — решение само закрепляет заявку за ней.
    o.press(ADMIN, &admin, "Вернуть", "Коротко причина").await;
    let preview = o
        .say(ADMIN, "клиент просит вернуть", "Вернуть клиенту?")
        .await;
    let taken = o.press(OWNER, &owner, "Взять", "уже взял(а)").await;
    o.press(OWNER, &taken, "Забрать себе", "Заявка ваша").await;
    o.press(ADMIN, &preview, "Вернуть", "уже взял(а)").await;
    o.drain().await;
    assert_eq!(order_state(&o, order.id).await, OrderState::ManualReview);
}

async fn orders_count(o: &Ops) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM orders")
        .fetch_one(&o.pool)
        .await
        .unwrap()
}

/// П-4 (аудит 16.11): ссылку на чек, которую сотрудник вставил в чат с ботом, обрабатывает пульт,
/// а не клиентский обмен — иначе юзербот открыл бы чек клиента и выплатил сотруднику. Любым
/// путём: текстом, голым кодом, `/start <код>`, правкой из инлайн-режима кошелька. Обмен как
/// клиент — только после кнопки, ровно одной ссылкой.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_check_link_from_staff_never_starts_a_client_exchange(pool: PgPool) {
    let o = ops(pool).await;
    o.say(ADMIN, "/menu", "<b>Пульт</b>").await;
    let url = o.world.client_check(CB, usdt(dec!(25))).unwrap();
    let code = parsers::links::parse_url(&url).unwrap().param;
    let unknown = o.say(ADMIN, &url, "Такого чека в заявках нет").await;
    o.say(ADMIN, &code, "Такого чека в заявках нет").await;
    o.say(
        ADMIN,
        &format!("/start {code}"),
        "Такого чека в заявках нет",
    )
    .await;
    // Сообщение «через @send»: ссылка приходит правкой — тоже пульту.
    let mark = o.tg.mark();
    o.tg.writes(ADMIN, "чек создаётся…");
    let message_id = {
        let s = o.tg.state.lock().unwrap();
        s.next_update - 1
    };
    o.tg.edits(ADMIN, message_id, &url);
    o.tg.screen(mark, ADMIN, "Такого чека в заявках нет").await;
    o.drain().await;
    assert_eq!(orders_count(&o).await, 0, "чек сотрудника не стал заявкой");
    assert!(
        o.world
            .check(&code)
            .is_some_and(|c| c.claimed_by.is_empty()),
        "чек никто не открывал"
    );

    // Чек из заявки клиента — её карточка.
    let client_url = o.world.client_check(CB, usdt(dec!(30))).unwrap();
    let order = accepted(
        o.engine
            .submit_check(
                common::CLIENT,
                &parsers::links::parse_url(&client_url).unwrap(),
            )
            .await
            .unwrap(),
    );
    let card = o.say(ADMIN, &client_url, "из заявки").await;
    assert!(card.text().contains(&order.public_id), "{}", card.text());
    // И поиск понимает ссылку.
    o.say(ADMIN, "/find", "Найти заявку").await;
    o.say(ADMIN, &client_url, &order.public_id).await;

    // «Обменять как клиент»: одна следующая ссылка уходит в обмен, следующая — снова пульту.
    o.press(
        ADMIN,
        &unknown,
        "Обменять как клиент",
        "Пришлите ссылку ещё раз",
    )
    .await;
    o.tg.writes(ADMIN, &url);
    for _ in 0..150 {
        if orders_count(&o).await == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(orders_count(&o).await, 2, "по кнопке — обычный обмен");
    let other = o.world.client_check(XR, usdt(dec!(5))).unwrap();
    o.say(ADMIN, &other, "Такого чека в заявках нет").await;
    assert_eq!(orders_count(&o).await, 2);
}

const OPERATOR: i64 = 503;

/// П-3 (аудит 16.11): оператор, который не может решить заявку с неясным исходом, видит «решает
/// админ» и «Позвать админа» вместо кнопок, которые ему недоступны. Его «Принял» на «Нужен
/// разбор» — тоже «Позвать админа»: повторы стихают у него, а у админа и владельца тревога
/// остаётся открытой и повторяется.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn an_operator_calls_an_admin_instead_of_silencing_everyone(pool: PgPool) {
    let o = ops_with(pool, |c| c.repeat_every = Duration::from_millis(300)).await;
    {
        let mut conn = o.pool.acquire().await.unwrap();
        storage::staff::add(&mut conn, OPERATOR, Role::Operator, "Олег", OWNER)
            .await
            .unwrap();
    }
    for who in [OWNER, ADMIN, OPERATOR] {
        o.say(who, "/menu", "<b>Пульт</b>").await;
    }
    // Активация без ответа кошелька: исход неясен, заявка на разборе.
    let url = o.world.client_check(XR, usdt(dec!(3))).unwrap();
    let order = accepted(
        o.engine
            .submit_check(common::CLIENT, &parsers::links::parse_url(&url).unwrap())
            .await
            .unwrap(),
    );
    o.world.inject(
        o.ub,
        userbot::sim::Call::SendText,
        userbot::sim::Fault::NoReply,
        1,
    );
    let mark = o.tg.mark();
    o.drain().await;
    o.bot.deliver_alerts().await.unwrap();
    let alert = o.tg.screen(mark, OPERATOR, "Нужен разбор").await;
    o.tg.screen(mark, ADMIN, "Нужен разбор").await;

    // Карточка у оператора.
    let card = o.press(OPERATOR, &alert, "Открыть", "Ручной разбор").await;
    assert!(card.text().contains("решает админ"), "{}", card.text());
    assert!(
        card.button("Позвать админа").is_some(),
        "{:?}",
        card.buttons()
    );
    assert!(card.button("Повторить").is_none() && card.button("Вернуть").is_none());

    // «Принял» оператора: его копия — «Позвал(а) админа», админу и владельцу — вызов.
    let alert = o.tg.screen(mark, OPERATOR, "Нужен разбор").await;
    let mark = o.tg.mark();
    let data = alert.button("Принял").expect("ack button");
    o.tg.presses(OPERATOR, alert.message_id.unwrap(), &data);
    o.tg.screen(mark, OPERATOR, "Позвал(а) админа Олег").await;
    let call = o.tg.screen(mark, ADMIN, "Олег зовёт админа").await;
    assert!(call.text().contains(&order.public_id), "{}", call.text());
    o.tg.screen(mark, OWNER, "Олег зовёт админа").await;
    let mut conn = o.pool.acquire().await.unwrap();
    let open = storage::alerts::open_by_key(&mut conn, &format!("manual_review:{}", order.id))
        .await
        .unwrap();
    assert!(open.is_some(), "тревога не погашена у всех");
    drop(conn);

    // Повтор: админу — да, оператору — нет.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let mark = o.tg.mark();
    o.bot.deliver_alerts().await.unwrap();
    o.tg.screen(mark, ADMIN, "Нужен разбор").await;
    let to_operator =
        o.tg.calls()
            .into_iter()
            .skip(mark)
            .filter(|c| c.chat() == Some(OPERATOR) && c.text().contains("Нужен разбор"))
            .count();
    assert_eq!(to_operator, 0, "оператору повторов больше нет");
    assert_eq!(o.audit("order.call_admin").await.len(), 1);
}

/// Веха 10 (макет «Новая авторизация»): чужой вход в аккаунт юзербота — тревога «Новый вход» с
/// устройством и скрытым IP; админ нажимает «Завершить сессию» — устройство выкинуто, тревога
/// закрыта, действие в журнале.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_foreign_login_is_shown_and_ended_from_the_panel(pool: PgPool) {
    let o = ops(pool).await;
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    o.say(ADMIN, "/menu", "<b>Пульт</b>").await;
    let s = |hash, current, device: &str| userbot::Session {
        hash,
        current,
        device: device.to_owned(),
        app: "Telegram iOS 11.2".to_owned(),
        country: "NL".to_owned(),
        ip: "185.12.34.41".to_owned(),
        created: 1_763_300_000,
    };
    o.world.set_sessions(o.ub, vec![s(1, true, "exch")]);
    o.engine.watch_sessions().await.unwrap();
    o.world
        .set_sessions(o.ub, vec![s(1, true, "exch"), s(7, false, "iPhone 15")]);
    o.engine.watch_sessions().await.unwrap();
    let mark = o.tg.mark();
    o.bot.deliver_alerts().await.unwrap();
    let alert = o.tg.screen(mark, ADMIN, "Новый вход").await;
    let text = alert.text();
    assert!(
        text.contains("ub-1") && text.contains("iPhone 15") && text.contains("185.…41"),
        "{text}"
    );
    assert!(!text.contains("185.12.34.41"), "IP скрыт: {text}");
    assert!(alert.button("Это я").is_some(), "{:?}", alert.buttons());
    o.press(
        ADMIN,
        &alert,
        "Завершить сессию",
        "Чужая сессия на ub-1 завершена",
    )
    .await;
    let left: Vec<i64> = o.world.sessions(o.ub).iter().map(|x| x.hash).collect();
    assert_eq!(left, vec![1]);
    let mut conn = o.pool.acquire().await.unwrap();
    assert!(
        storage::alerts::open_by_key(&mut conn, "new_login:ub-1:7")
            .await
            .unwrap()
            .is_none(),
        "тревога закрыта"
    );
    drop(conn);
    assert_eq!(o.audit("userbot.end_session").await.len(), 1);
}

/// П-19.3 (проверка 19.11): копию решённой тревоги у сотрудника, заблокировавшего бота, поправить
/// нельзя (403) — она закрывается с первого раза. Раньше такие копии оставались в очереди «Решено»
/// навсегда: 50 самых старых каждый круг пробовались снова, а у остальных решённое висело с кнопками.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn copies_in_a_chat_that_blocked_the_bot_do_not_hold_the_resolved_queue(pool: PgPool) {
    let o = ops(pool).await;
    let mut conn = o.pool.acquire().await.unwrap();
    async fn raise(conn: &mut sqlx::PgConnection, key: &str) -> i64 {
        storage::alerts::raise(
            conn,
            &storage::alerts::NewAlert {
                level: storage::alerts::Level::Warn,
                topic: storage::alerts::Topic::Errors,
                dedup_key: key,
                title: "reserve_low",
                payload: json!({"platform": "xrocket"}),
            },
        )
        .await
        .unwrap()
    }
    // 50 старых тревог — копии у бывшего оператора; потом одна — у владельца.
    for n in 0..50 {
        let id = raise(&mut conn, &format!("old:{n}")).await;
        storage::alerts::save_copy(&mut conn, id, OPERATOR, 100 + n)
            .await
            .unwrap();
        storage::alerts::mark_delivered(&mut conn, id, Some(100 + n))
            .await
            .unwrap();
    }
    let owner_alert = raise(&mut conn, "fresh").await;
    storage::alerts::save_copy(&mut conn, owner_alert, OWNER, 7)
        .await
        .unwrap();
    storage::alerts::mark_delivered(&mut conn, owner_alert, Some(7))
        .await
        .unwrap();
    // Бывший оператор заблокировал бота раньше, чем тревоги решились: фоновый круг тревог не
    // должен успеть поправить его копии.
    o.tg.lock().blocked.insert(OPERATOR);
    for n in 0..50 {
        storage::alerts::resolve(&mut conn, &format!("old:{n}"))
            .await
            .unwrap();
    }
    storage::alerts::resolve(&mut conn, "fresh").await.unwrap();
    drop(conn);

    let mark = o.tg.mark();
    o.bot.deliver_alerts().await.unwrap();
    o.bot.deliver_alerts().await.unwrap();
    let owner_closed = o.tg.calls().into_iter().skip(mark).any(|c| {
        c.method == "editMessageText"
            && c.chat() == Some(OWNER)
            && c.body["message_id"].as_i64() == Some(7)
    });
    assert!(owner_closed, "копия владельца стала «Решено»");
    let tries = o.tg.lock().refused.len();
    assert_eq!(
        tries, 50,
        "каждая копия в заблокированном чате — одна попытка"
    );
}

/// Д-19.9 в пульте: «Снять паузу» в тревоге «Незнакомый ответ» снимает только эту причину. У
/// направления xR → CB может стоять и пауза по недостаче в CryptoBot — она остаётся, выплаты из
/// кошелька, где не хватает денег, не идут. Раньше кнопка снимала паузу целиком.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn unpausing_from_an_unknown_reply_keeps_a_shortfall_pause(pool: PgPool) {
    let o = ops(pool).await;
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    let short = "сверка: в кошельке 988 USDT, ожидалось 1000 — не хватает 12";
    {
        let mut conn = o.pool.acquire().await.unwrap();
        storage::directions::auto_pause_platform(
            &mut conn,
            XR,
            storage::directions::Side::Source,
            engine::UNKNOWN_REPLY_PAUSE,
        )
        .await
        .unwrap();
        storage::directions::auto_pause_platform(
            &mut conn,
            CB,
            storage::directions::Side::Target,
            short,
        )
        .await
        .unwrap();
        storage::alerts::raise(
            &mut conn,
            &storage::alerts::NewAlert {
                level: storage::alerts::Level::Critical,
                topic: storage::alerts::Topic::Errors,
                dedup_key: "unknown_reply:xrocket:1",
                title: "unknown_reply",
                payload: json!({"platform": "xrocket", "text": "Чек временно недоступен",
                    "public": "7F3A1C9D2B", "direction": "xr_to_cb_check",
                    "at": "2026-11-20T11:02:00+00:00"}),
            },
        )
        .await
        .unwrap();
    }
    let mark = o.tg.mark();
    o.bot.deliver_alerts().await.unwrap();
    let alert = o.tg.screen(mark, OWNER, "Незнакомый ответ").await;
    let preview = o
        .press(OWNER, &alert, "Снять паузу", "проверьте изменение")
        .await;
    o.press(OWNER, &preview, "Подтвердить", "xRocket").await;
    let mut conn = o.pool.acquire().await.unwrap();
    for d in [Direction::XrToCbCheck, Direction::PayCbInvoice] {
        let row = storage::directions::get(&mut conn, d).await.unwrap();
        assert!(row.auto_paused, "{d:?}: недостача CryptoBot держит паузу");
        assert_eq!(row.pause_reason.as_deref(), Some(short), "{d:?}");
    }
    let unknown: i64 =
        sqlx::query_scalar("SELECT count(*) FROM direction_pauses WHERE reason = $1")
            .bind(engine::UNKNOWN_REPLY_PAUSE)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(unknown, 0, "пауза «незнакомый ответ» снята");
}

/// Б-19.3 (проверка 19.11): «Принял» («Это я») и показ тревоги по номеру — только тем, кому она
/// приходит. Раньше оператор, которому «Новый вход» не приходит, присылал `ack:<номер>` — у
/// владельца и админа повторы стихали, — а `al:<номер>` показывал ему устройство и страну.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn an_operator_can_neither_ack_nor_open_an_alert_meant_for_admins(pool: PgPool) {
    let o = ops(pool).await;
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    o.say(ADMIN, "/menu", "<b>Пульт</b>").await;
    let id = {
        let mut conn = o.pool.acquire().await.unwrap();
        storage::staff::add(&mut conn, OPERATOR, Role::Operator, "Олег", OWNER)
            .await
            .unwrap();
        storage::alerts::raise(
            &mut conn,
            &storage::alerts::NewAlert {
                level: storage::alerts::Level::Critical,
                topic: storage::alerts::Topic::Security,
                dedup_key: "new_login:ub-1:7",
                title: "new_login",
                payload: json!({"label": "ub-1", "hash": 7, "device": "iPhone 15",
                    "app": "Telegram iOS 11.2", "country": "NL", "ip": "185.…41"}),
            },
        )
        .await
        .unwrap()
    };
    let mark = o.tg.mark();
    o.bot.deliver_alerts().await.unwrap();
    o.tg.screen(mark, ADMIN, "Новый вход").await;
    o.say(OPERATOR, "/menu", "<b>Пульт</b>").await;

    // Подделанные кнопки оператора: показ и «Это я».
    let mark = o.tg.mark();
    o.tg.presses(OPERATOR, 1, &format!("a:al:{id}"));
    o.tg.presses(OPERATOR, 1, &format!("a:ack:{id}"));
    o.say(OPERATOR, "/menu", "<b>Пульт</b>").await;
    let to_operator: Vec<String> =
        o.tg.calls()
            .into_iter()
            .skip(mark)
            .filter(|c| c.chat() == Some(OPERATOR))
            .map(|c| c.text())
            .collect();
    assert!(
        to_operator.iter().all(|t| !t.contains("iPhone 15")),
        "оператору тревога не показана: {to_operator:#?}"
    );
    assert!(
        o.tg.calls()
            .iter()
            .skip(mark)
            .all(|c| !c.text().contains("повторы остановлены")),
        "копии владельца и админа не тронуты"
    );
    let mut conn = o.pool.acquire().await.unwrap();
    let alert = storage::alerts::get(&mut conn, id).await.unwrap().unwrap();
    assert!(alert.acked_at.is_none(), "тревога не принята оператором");

    // Недостачу оператор видит на главном экране и в «Резервах» — её кнопка «Недостача» открывает.
    let short = storage::alerts::raise(
        &mut conn,
        &storage::alerts::NewAlert {
            level: storage::alerts::Level::Critical,
            topic: storage::alerts::Topic::Security,
            dedup_key: "balance_mismatch:xrocket",
            title: "balance_mismatch",
            payload: json!({"platform": "xrocket", "diff": "-12", "expected": "640",
                "actual": "628"}),
        },
    )
    .await
    .unwrap();
    drop(conn);
    let mark = o.tg.mark();
    o.tg.presses(OPERATOR, 1, &format!("a:al:{short}"));
    o.tg.screen(mark, OPERATOR, "меньше на").await;
}

/// П-26 (аудит 16.11): «Реально» красное только при открытой недостаче. Раньше сверку
/// десятиминутной давности сравнивали с текущим леджером — после каждого приёма между сверками
/// загоралась красная точка, будто денег меньше.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn reserves_turn_red_only_on_a_real_shortage(pool: PgPool) {
    let o = ops(pool).await;
    o.engine.reconcile_balances().await.unwrap();
    // Клиент поменял 100 CB → xR: леджер CryptoBot вырос, новой сверки ещё не было.
    let url = o.world.client_check(CB, usdt(dec!(100))).unwrap();
    accepted(
        o.engine
            .submit_check(common::CLIENT, &parsers::links::parse_url(&url).unwrap())
            .await
            .unwrap(),
    );
    o.drain().await;
    let dash = o.say(OWNER, "/dash", "Резервы").await;
    let reserves = o.press(OWNER, &dash, "Резервы", "Цель · порог").await;
    assert!(!reserves.text().contains("🔴"), "{}", reserves.text());
    // Настоящая недостача — красное.
    o.world
        .set_balance(o.ub, XR, Asset::Usdt, dec!(500))
        .unwrap();
    o.engine.reconcile_balances().await.unwrap();
    let dash = o.say(OWNER, "/dash", "Резервы").await;
    let reserves = o.press(OWNER, &dash, "Резервы", "Цель · порог").await;
    assert!(reserves.text().contains("🔴"), "{}", reserves.text());
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn client_card_sets_a_personal_fee(pool: PgPool) {
    let o = ops(pool).await;
    let card = o
        .say(OWNER, &format!("/user {}", common::CLIENT), "Клиент")
        .await;
    let dirs = o
        .press(OWNER, &card, "Личные условия", "Выберите направление")
        .await;
    let dir = o.press(OWNER, &dirs, "CryptoBot → xRocket", "Общие").await;
    o.press(OWNER, &dir, "Комиссия", "Личная комиссия").await;
    let preview = o.say(OWNER, "1", "проверьте").await;
    let dir = o.press(OWNER, &preview, "Подтвердить", "Сохранено").await;
    assert!(dir.text().contains("комиссия 1\u{a0}%"));
    let mut conn = o.pool.acquire().await.unwrap();
    let terms = storage::user_terms::get(&mut conn, common::CLIENT, Direction::CbToXrCheck)
        .await
        .unwrap();
    assert_eq!(terms.fee_pct, Some(dec!(1)));
    assert_eq!(terms.updated_by, Some(OWNER));
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn owner_top_up_is_posted_from_the_reconciliation_alert(pool: PgPool) {
    let o = ops(pool).await;
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    o.say(ADMIN, "/menu", "<b>Пульт</b>").await;
    // Владелец пополнил CryptoBot на 250 — леджер об этом не знает.
    o.world
        .set_balance(o.ub, CB, Asset::Usdt, dec!(1250))
        .unwrap();
    let mark = o.tg.mark();
    o.engine.reconcile_balances().await.unwrap();
    // Макет «Недостача» (здесь — излишек): «больше на 250.00 … Это вы пополняли?».
    let alert = o.tg.screen(mark, OWNER, "больше на").await;
    assert!(alert.text().contains("250.00"), "{}", alert.text());
    assert!(
        alert.text().contains("ub-1"),
        "аккаунт кошелька: {}",
        alert.text()
    );
    // Админу провести нельзя — только владельцу.
    let admin_alert = o.tg.screen(mark, ADMIN, "больше на").await;
    o.press(ADMIN, &admin_alert, "Да, это я", "роли «Владелец»")
        .await;
    let preview = o.press(OWNER, &alert, "Да, это я", "взнос владельца").await;
    o.press(OWNER, &preview, "Подтвердить", "Сохранено").await;
    let r = o.engine.reconcile_balances().await.unwrap();
    assert!(
        r.iter()
            .all(|x| x.verdict == engine::reconcile::Verdict::Ok),
        "{r:?}"
    );
    assert_eq!(o.audit("capital.in").await.len(), 1);
}

/// Картинка «Списать как потерю» (принята 18.11): недостачу, которую не объяснить, владелец
/// признаёт потерей — проводка `loss` с причиной, событие закрыто, сверка сходится, прибыль
/// инвесторов за период уменьшится (решение 17.11). Админу — нельзя.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn an_unexplained_shortage_is_written_off_as_a_loss(pool: PgPool) {
    let o = ops(pool).await;
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    o.say(ADMIN, "/menu", "<b>Пульт</b>").await;
    o.world
        .set_balance(o.ub, XR, Asset::Usdt, dec!(970))
        .unwrap();
    let mark = o.tg.mark();
    o.engine.reconcile_balances().await.unwrap();
    let alert = o.tg.screen(mark, OWNER, "меньше на").await;
    let admin_alert = o.tg.screen(mark, ADMIN, "меньше на").await;
    o.press(ADMIN, &admin_alert, "Списать как потерю", "роли «Владелец»")
        .await;
    o.press(OWNER, &alert, "Списать как потерю", "Коротко причина")
        .await;
    let preview = o
        .say(
            OWNER,
            "аккаунт заблокирован, деньги не вернуть",
            "как потерю?",
        )
        .await;
    assert!(preview.text().contains("30.00"), "{}", preview.text());
    let confirm = preview.body["reply_markup"]["inline_keyboard"][0][0].clone();
    assert_eq!(confirm["style"], "danger", "{confirm}");
    assert!(
        confirm["text"].as_str().unwrap().contains("Списать 30.00"),
        "{confirm}"
    );
    o.press(OWNER, &preview, "Списать 30.00", "Сохранено").await;
    let mut conn = o.pool.acquire().await.unwrap();
    let loss: domain::Decimal = sqlx::query_scalar(
        "SELECT COALESCE(sum(e.amount), 0) FROM ledger_entries e \
           JOIN ledger_accounts a ON a.id = e.account_id WHERE a.code = 'expense:loss:USDT'",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(loss, dec!(30));
    drop(conn);
    let r = o.engine.reconcile_balances().await.unwrap();
    assert!(
        r.iter()
            .all(|x| x.verdict == engine::reconcile::Verdict::Ok),
        "{r:?}"
    );
    assert_eq!(o.audit("capital.loss").await.len(), 1);
}

/// Д-19.10 (картинка `passcode-limit.png` принята владельцем 20.11): лимит без код-пароля
/// CryptoBot — в «Резервах». Строка «Без код-пароля» — лимит и сколько осталось на сутки; лимит
/// кончается — строкой ниже, что встало и до скольки; кнопка «Без код-пароля» — только владельцу:
/// было → стало, «Сохранить», запись в журнал. Раньше лимит поставить было негде (всегда $500).
/// Д-19.6 (проверка 19.11, картинка last-fixes.png принята 20.11): «Персонал → Добавить» с ID
/// владельца и ролью admin молча понижал владельца до админа. Теперь — отказ, роль та же.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn adding_the_owner_as_staff_is_refused(pool: PgPool) {
    let o = ops(pool).await;
    let menu = o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    let staff = o.press(OWNER, &menu, "Персонал", "Персонал").await;
    let ask = o.press(OWNER, &staff, "Добавить", "ID в Telegram").await;
    assert!(ask.button("Отмена").is_some(), "{:?}", ask.buttons());
    let refused = o
        .say(OWNER, &format!("{OWNER} admin Иван"), "это владелец")
        .await;
    assert!(
        refused.text().contains(&format!(
            "<code>{OWNER}</code> — это владелец, его роль не меняю"
        )),
        "{}",
        refused.text()
    );
    assert!(
        refused.button("Подтвердить").is_none(),
        "{:?}",
        refused.buttons()
    );
    let mut conn = o.pool.acquire().await.unwrap();
    let owner = storage::staff::get(&mut conn, OWNER)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owner.role, Role::Owner, "владелец остался владельцем");
    assert!(
        !storage::staff::set_role(&mut conn, OWNER, Role::Owner, Role::Admin)
            .await
            .unwrap()
    );
    assert!(
        !storage::staff::set_role(&mut conn, ADMIN, Role::Admin, Role::Owner)
            .await
            .unwrap()
    );
    assert!(
        !storage::staff::disable(&mut conn, OWNER, Role::Owner)
            .await
            .unwrap()
    );
    assert!(
        !storage::staff::set_manual_payout(&mut conn, OWNER, Role::Owner, None, Some(dec!(50)))
            .await
            .unwrap()
    );
    // И в базе: добавление с ID владельца роль владельца не перезаписывает.
    storage::staff::add(&mut conn, OWNER, Role::Admin, "Иван", ADMIN)
        .await
        .unwrap();
    let owner = storage::staff::get(&mut conn, OWNER)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owner.role, Role::Owner, "add не понижает владельца");
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn the_owner_sets_the_passcode_free_limit_in_reserves(pool: PgPool) {
    let o = ops(pool).await;
    {
        let mut conn = o.pool.acquire().await.unwrap();
        storage::settings::set(&mut conn, "cb_passcode_free_limit_usd", &json!(500), None)
            .await
            .unwrap();
    }
    let admin_dash = o.say(ADMIN, "/dash", "Резервы").await;
    let admin_reserves = o
        .press(ADMIN, &admin_dash, "Резервы", "Без код-пароля")
        .await;
    assert!(
        admin_reserves.button("Без код-пароля").is_none(),
        "менять лимит — только владельцу: {:?}",
        admin_reserves.buttons()
    );
    let dash = o.say(OWNER, "/dash", "Резервы").await;
    let reserves = o.press(OWNER, &dash, "Резервы", "Без код-пароля").await;
    let text = reserves.text();
    assert!(text.contains("500 · осталось 475"), "{text}");
    assert!(
        text.contains("до лимита без код-пароля осталось 475.00"),
        "лимит меньше резерва — строка, что встало: {text}"
    );
    let ask = o
        .press(OWNER, &reserves, "Без код-пароля", "Лимит без код-пароля")
        .await;
    assert!(
        ask.text().contains("Сейчас <code>500</code>"),
        "{}",
        ask.text()
    );
    let preview = o.say(OWNER, "30000", "за сутки exch потратит").await;
    let p = preview.text();
    assert!(
        p.contains("<code>500</code> → <code>30\u{a0}000</code>") && p.contains("28\u{a0}500"),
        "{p}"
    );
    assert!(p.contains("Поставьте то же число в CryptoBot"), "{p}");
    let after = o
        .press(OWNER, &preview, "Сохранить", "Без код-пароля")
        .await;
    assert!(
        after.text().contains("30\u{a0}000 · осталось 28\u{a0}500"),
        "{}",
        after.text()
    );
    assert!(!after.text().contains("до лимита без код-пароля осталось"));
    let mut conn = o.pool.acquire().await.unwrap();
    let saved = storage::settings::get(&mut conn, "cb_passcode_free_limit_usd")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved, json!("30000"));
    drop(conn);
    assert_eq!(o.audit("settings.passcode_free_limit").await.len(), 1);
}

/// П-19.2 (картинка link-missing.png принята 20.11): «Нет ссылки на чек» — только владельцу и без
/// звука, русским текстом; «Сохранить фикстуру» — устройство сообщения без ссылок. Раньше —
/// всем со звуком, заголовок `check_link_missing` и строка JSON.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_missing_check_link_goes_to_the_owner_quietly(pool: PgPool) {
    let o = ops(pool).await;
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    o.say(ADMIN, "/menu", "<b>Пульт</b>").await;
    {
        let mut conn = o.pool.acquire().await.unwrap();
        storage::alerts::raise(
            &mut conn,
            &storage::alerts::NewAlert {
                level: storage::alerts::Level::Warn,
                topic: storage::alerts::Topic::Errors,
                dedup_key: "check_link_missing:111:80",
                title: "check_link_missing",
                payload: json!({"user_id": 111, "platform": "cryptobot",
                    "shape": {"via_bot": "send", "edited": true, "text_len": 3,
                        "buttons": ["url:https://t.me/send"]}}),
            },
        )
        .await
        .unwrap();
    }
    let mark = o.tg.mark();
    o.bot.deliver_alerts().await.unwrap();
    let alert = o.tg.screen(mark, OWNER, "Нет ссылки на чек").await;
    assert_eq!(alert.body["disable_notification"], true, "без звука");
    let text = alert.text();
    assert!(
        text.contains("@send · правкой · текст 3 знака · кнопки: url:https://t.me/send"),
        "{text}"
    );
    assert!(
        !text.contains("check_link_missing") && !text.contains("{\""),
        "{text}"
    );
    assert!(
        o.tg.calls()
            .iter()
            .skip(mark)
            .all(|c| c.chat() != Some(ADMIN) || !c.text().contains("Нет ссылки")),
        "админу не приходит"
    );
    let dev = o
        .press(OWNER, &alert, "Сохранить фикстуру", "Для разработчика")
        .await;
    assert!(dev.text().contains("text_len"), "{}", dev.text());
}

/// П-19.4 (проверка 19.11): «Сверить сейчас» идёт в фоне — пока кошельки отвечают на запрос
/// баланса, клиенты получают ответы. Раньше сверка шла внутри опроса Telegram: следующую порцию
/// обновлений бот забирал только после неё, и все клиенты ждали.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn clients_are_answered_while_the_reconciliation_runs(pool: PgPool) {
    let o = ops(pool).await;
    let dash = o.say(OWNER, "/dash", "Резервы").await;
    let reserves = o.press(OWNER, &dash, "Резервы", "Сверить сейчас").await;
    // Каждый кошелёк отвечает на баланс не сразу (но в пределах ожидания ответа).
    o.world.inject(
        o.ub,
        userbot::sim::Call::Any,
        userbot::sim::Fault::DelayReply(Duration::from_millis(250)),
        2,
    );
    let mark = o.tg.mark();
    let data = reserves.button("Сверить сейчас").unwrap();
    o.tg.presses(OWNER, reserves.message_id.unwrap(), &data);
    tokio::time::sleep(Duration::from_millis(100)).await;
    o.tg.writes(common::CLIENT, "/start");
    o.tg.screen(mark, common::CLIENT, "Обмен CryptoBot").await;
    o.tg.screen(mark, OWNER, "Сверено").await;
    let calls = o.tg.calls();
    let at = |chat: i64, needle: &str| {
        calls
            .iter()
            .skip(mark)
            .position(|c| c.chat() == Some(chat) && c.text().contains(needle))
            .unwrap()
    };
    assert!(
        at(common::CLIENT, "Обмен CryptoBot") < at(OWNER, "Сверено"),
        "клиент получил ответ, не дожидаясь конца сверки"
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn selftest_from_the_panel_reports_every_step(pool: PgPool) {
    let o = ops(pool).await;
    // Макет убрал самопроверку с главного экрана: она — в «Аккаунтах».
    let dash = o.say(OWNER, "/dash", "Всё спокойно").await;
    let bots = o.press(OWNER, &dash, "Аккаунты", "Юзерботы").await;
    // П-14: статус аккаунта — словами, а не кодом из базы.
    assert!(
        bots.text().contains("ub-1: ещё не вошёл") && !bots.text().contains(": new"),
        "{}",
        bots.text()
    );
    let preview = o
        .press(OWNER, &bots, "Самопроверка", "сам же его активирует")
        .await;
    // П-19.4: самопроверка ждёт кошельки — клиент тем временем получает ответ.
    o.world.inject(
        o.ub,
        userbot::sim::Call::Any,
        userbot::sim::Fault::DelayReply(Duration::from_millis(250)),
        2,
    );
    let mark = o.tg.mark();
    let data = preview.button("Подтвердить").unwrap();
    o.tg.presses(OWNER, preview.message_id.unwrap(), &data);
    tokio::time::sleep(Duration::from_millis(100)).await;
    o.tg.writes(common::CLIENT, "/start");
    o.tg.screen(mark, common::CLIENT, "Обмен CryptoBot").await;
    let report = o.tg.screen(mark, OWNER, "Всё работает").await;
    assert!(!report.text().contains("❌"), "{}", report.text());
    let calls = o.tg.calls();
    let at = |chat: i64, needle: &str| {
        calls
            .iter()
            .skip(mark)
            .position(|c| c.chat() == Some(chat) && c.text().contains(needle))
            .unwrap()
    };
    assert!(at(common::CLIENT, "Обмен CryptoBot") < at(OWNER, "Всё работает"));
    for platform in [CB, XR] {
        assert_eq!(o.world.balance(o.ub, platform, Asset::Usdt), dec!(1000));
    }
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn owner_grants_investor_access_and_admin_approves_a_withdrawal(pool: PgPool) {
    let o = ops(pool).await;
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    o.say(ADMIN, "/menu", "<b>Пульт</b>").await;

    // Админ видит карточку, но выдать доступ не может — кнопки нет.
    let card = o
        .say(ADMIN, &format!("/user {}", common::CLIENT), "Клиент")
        .await;
    let invest = o.press(ADMIN, &card, "Инвестиции", "Доступа нет").await;
    assert!(invest.button("Дать доступ").is_none());

    // Владелец: «Дать доступ» → условия одной строкой → предпросмотр → подтвердить.
    let card = o
        .say(OWNER, &format!("/user {}", common::CLIENT), "Клиент")
        .await;
    let invest = o.press(OWNER, &card, "Инвестиции", "Доступа нет").await;
    o.press(OWNER, &invest, "Дать доступ", "одной строкой")
        .await;
    let bad = o.say(OWNER, "500 0 10", "Не понял формат").await;
    assert!(bad.text().contains("доля от 0 до 100"));
    let preview = o.say(OWNER, "50 0 10", "Дать доступ").await;
    let card = o.press(OWNER, &preview, "Подтвердить", "Сохранено").await;
    assert!(card.text().contains("50\u{a0}%"), "{}", card.text());
    assert_eq!(o.audit("invest.terms").await.len(), 1);

    // Инвестор пополнил на 100 и просит 40 — выше автолимита 10: решение за персоналом.
    let url = o.world.client_check(CB, usdt(dec!(100))).unwrap();
    let link = parsers::links::parse_url(&url).unwrap();
    o.engine
        .invest_deposit(common::CLIENT, &link)
        .await
        .unwrap();
    o.drain().await;
    let mark = o.tg.mark();
    let w = o
        .engine
        .invest_withdraw(common::CLIENT, XR, dec!(40))
        .await
        .unwrap();
    assert_eq!(w.status, storage::invest::MoveStatus::Requested);
    let alert = o.tg.screen(mark, ADMIN, "Заявка на вывод").await;
    let preview = o.press(ADMIN, &alert, "Одобрить", "Одобрить вывод").await;
    o.press(ADMIN, &preview, "Подтвердить", "Готово").await;
    o.drain().await;
    let mut conn = o.pool.acquire().await.unwrap();
    let m = storage::invest::get(&mut conn, w.id, false).await.unwrap();
    assert_eq!(m.status, storage::invest::MoveStatus::Done);
    assert_eq!(m.decided_by, Some(ADMIN));
    assert_eq!(o.world.checks_created_by(o.ub).len(), 1, "один чек вывода");
    assert_eq!(o.audit("invest.approve").await.len(), 1);
}

/// Д-19.9 (картинка last-fixes.png принята 20.11): карточка направления показывает все причины
/// автопаузы — у каждой время и кошелёк. Раньше видна была только самая ранняя.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn the_direction_card_lists_every_pause_reason(pool: PgPool) {
    let o = ops(pool).await;
    let short = "сверка: в кошельке 628.00 USDT, ожидалось 640.00 — не хватает 12.00";
    {
        let mut conn = o.pool.acquire().await.unwrap();
        storage::directions::auto_pause_platform(
            &mut conn,
            XR,
            storage::directions::Side::Source,
            engine::UNKNOWN_REPLY_PAUSE,
        )
        .await
        .unwrap();
        storage::directions::auto_pause_platform(
            &mut conn,
            CB,
            storage::directions::Side::Target,
            short,
        )
        .await
        .unwrap();
    }
    let dirs = o.say(OWNER, "/dir", "Направления").await;
    let card = o.press(OWNER, &dirs, "xR → CB", "Статус").await;
    let text = card.text();
    assert!(
        text.contains(&format!("xRocket — {}", engine::UNKNOWN_REPLY_PAUSE)),
        "{text}"
    );
    assert!(text.contains(&format!("CryptoBot — {short}")), "{text}");
    assert_eq!(text.matches("· с ").count(), 2, "две причины: {text}");
}

/// Д-19.6 (картинка last-fixes.png принята 20.11, вариант Б): чек возврата пополнения меньше
/// минимума не создался (исход неясен) — у владельца выбор «Чек создан» / «Ничего не было», как у
/// вывода, а не «Деньги пришли» с вопросом о сумме. «Ничего не было» — новый чек, один.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn an_unclear_deposit_return_asks_whether_the_check_was_made(pool: PgPool) {
    use userbot::sim::{Call as SimCall, Fault};
    let o = ops(pool).await;
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    {
        let mut conn = o.pool.acquire().await.unwrap();
        let owner = storage::staff::get(&mut conn, OWNER)
            .await
            .unwrap()
            .unwrap();
        drop(conn);
        o.engine
            .invest_save_terms(
                &owner,
                &storage::invest::Investor {
                    user_id: common::CLIENT,
                    share_pct: dec!(50),
                    lock_days: 0,
                    min_deposit: dec!(10),
                    auto_withdraw_max: dec!(10),
                    is_active: true,
                    granted_by: None,
                    created_at: chrono::Utc::now(),
                },
            )
            .await
            .unwrap();
    }
    o.world
        .inject(o.ub, SimCall::CheckPress, Fault::TimeoutBeforeEffect, 1);
    let url = o.world.client_check(CB, usdt(dec!(5))).unwrap();
    let link = parsers::links::parse_url(&url).unwrap();
    let id = o
        .engine
        .invest_deposit(common::CLIENT, &link)
        .await
        .unwrap();
    let mark = o.tg.mark();
    o.drain().await;
    o.engine.resolve_unknown().await.unwrap();
    o.drain().await;
    let alert = o.tg.screen(mark, OWNER, "Инвестиции: нужен человек").await;
    let card = o
        .press(OWNER, &alert, "Открыть", &format!("Движение #{id}"))
        .await;
    assert!(card.button("Чек создан").is_some(), "{:?}", card.buttons());
    assert!(
        card.button("Деньги пришли").is_none(),
        "{:?}",
        card.buttons()
    );
    o.press(OWNER, &card, "Ничего не было", "что вы проверили")
        .await;
    let preview = o
        .say(
            OWNER,
            "проверил «Мои чеки» — чека нет",
            "чека возврата нет — создам новый чек",
        )
        .await;
    o.press(OWNER, &preview, "Подтвердить", "Готово").await;
    o.drain().await;
    let mut conn = o.pool.acquire().await.unwrap();
    let m = storage::invest::get(&mut conn, id, false).await.unwrap();
    assert_eq!(m.status, storage::invest::MoveStatus::Refunded, "{m:?}");
    assert_eq!(
        o.world.checks_created_by(o.ub).len(),
        1,
        "один чек возврата"
    );
}

/// Сводка говорит, почему направление не принимает; «Сверить балансы» сразу предлагает
/// провести пополнение владельца; нечитаемый баланс — событие с причиной.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn dash_explains_pauses_and_reconcile_offers_the_top_up(pool: PgPool) {
    let o = ops(pool).await;
    sqlx::query(
        "UPDATE directions SET min_amount = 2000, max_amount = 5000 WHERE code = 'cb_to_xr_check'",
    )
    .execute(&o.pool)
    .await
    .unwrap();
    // Почему направление не принимает — в «Направлениях» (главный экран по макету
    // показывает только то, что ждёт человека).
    let dirs = o.say(OWNER, "/dir", "Направления").await;
    assert!(
        dirs.text().contains("нет резерва: в xRocket свободно"),
        "{}",
        dirs.text()
    );
    assert!(dirs.text().contains("2–300"), "{}", dirs.text());

    // Владелец пополнил xRocket на 20 — «Резервы» → «Сверить сейчас» показывают излишек,
    // а по кнопке — событие «Это вы пополняли?».
    o.world
        .set_balance(o.ub, XR, Asset::Usdt, dec!(1020))
        .unwrap();
    let dash = o.say(OWNER, "/dash", "Всё спокойно").await;
    let reserves = o.press(OWNER, &dash, "Резервы", "Цель · порог").await;
    assert_eq!(
        reserves.method, "editMessageText",
        "карточка в том же сообщении"
    );
    let fresh = o
        .press(
            OWNER,
            &reserves,
            "Сверить сейчас",
            "расхождение xRocket +20.00",
        )
        .await;
    assert!(fresh.text().contains("Сверено"), "{}", fresh.text());
    assert!(
        fresh.text().contains("1\u{a0}020.00"),
        "реально: {}",
        fresh.text()
    );
    let alert = o
        .press(OWNER, &fresh, "Излишек xRocket", "Это вы пополняли?")
        .await;
    let preview = o.press(OWNER, &alert, "Да, это я", "взнос владельца").await;
    o.press(OWNER, &preview, "Подтвердить", "Сохранено").await;
    let r = o.engine.reconcile_balances().await.unwrap();
    assert!(
        r.iter()
            .all(|x| x.verdict == engine::reconcile::Verdict::Ok),
        "{r:?}"
    );

    // Кошелёк не отвечает — владелец получает событие с причиной, а не тишину.
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    o.world.inject(
        o.ub,
        userbot::sim::Call::SendText,
        userbot::sim::Fault::NoReply,
        2,
    );
    let mark = o.tg.mark();
    o.engine.reconcile_balances().await.unwrap();
    let alert = o.tg.screen(mark, OWNER, "не читается").await;
    assert!(
        alert.button("Сверить снова").is_some(),
        "{:?}",
        alert.buttons()
    );
    assert!(alert.text().contains("Баланс"), "{}", alert.text());
}

/// Макет «Обращение»: вопрос клиента из «Поддержки» приходит персоналу с кнопкой «Ответить»,
/// ответ сотрудника уходит клиенту от имени бота с его именем.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn support_question_reaches_staff_and_the_answer_reaches_the_client(pool: PgPool) {
    use tgbot::UpdateHook as _;
    let o = ops(pool).await;
    let client = botapi::User {
        id: 4242,
        is_bot: false,
        first_name: "Клиент".into(),
        last_name: None,
        username: Some("nick".into()),
        language_code: None,
        is_premium: None,
    };
    let mark = o.tg.mark();
    assert!(
        o.bot
            .support_question(&client, "чек не пришёл <уже 10 минут>", Some("K7M2Q9XR4T"))
            .await
    );
    let question = o.tg.screen(mark, ADMIN, "Обращение").await;
    assert!(question.text().contains("@nick"), "{}", question.text());
    assert!(
        question.text().contains("&lt;уже 10 минут&gt;"),
        "чужой текст экранирован"
    );
    assert!(question.text().contains("K7M2Q9XR4T"));
    let owner_copy = o.tg.screen(mark, OWNER, "Обращение").await;
    // П-10: обращения отвечают операторы и админы; владельцу — тихо.
    assert_eq!(owner_copy.body["disable_notification"], true);
    assert!(question.body["disable_notification"].is_null());
    // В меню — счётчик и синяя кнопка: ждут человека (макет «Меню пульта»).
    let menu = o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    assert!(menu.button("Обращения · 1").is_some(), "{:?}", menu.body);
    let inbox = o
        .press(OWNER, &menu, "Обращения · 1", "Обращения · 1")
        .await;
    assert!(inbox.text().contains("4242"), "{}", inbox.text());

    // П-8: вопрос не пропадает с экрана — просьба написать ответ приходит новым сообщением
    // ответом на карточку обращения, сама карточка цела.
    let prompt = o
        .press(ADMIN, &question, "Ответить", "Ответ клиенту 4242")
        .await;
    assert_eq!(prompt.method, "sendMessage");
    assert_eq!(
        prompt.body["reply_parameters"]["message_id"].as_i64(),
        question.message_id,
        "{}",
        prompt.body
    );
    let edited =
        o.tg.calls()
            .into_iter()
            .filter(|c| c.method.starts_with("edit") && c.chat() == Some(ADMIN))
            .any(|c| c.body["message_id"].as_i64() == question.message_id);
    assert!(!edited, "карточку обращения не правили");
    o.say(
        ADMIN,
        "деньги у нас, чек будет через 5 минут",
        "Отправлено клиенту · Анна",
    )
    .await;
    // Вид — «Ответ поддержки» (полировка 16.11); «Ответить» снова открывает поддержку по
    // той же заявке (Д7/С3, К-2): раньше ответ клиента получал «Пришлите чек…».
    let answer = o.tg.screen(mark, 4242, "Ответ поддержки").await;
    assert!(answer.text().contains("деньги у нас"), "{}", answer.text());
    assert!(
        answer.text().contains("Анна, оператор"),
        "{}",
        answer.text()
    );
    assert_eq!(
        answer.button("Ответить").as_deref(),
        Some("support:K7M2Q9XR4T"),
        "{:?}",
        answer.buttons()
    );
    // Ответили — обращение закрыто, счётчика больше нет.
    let menu = o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    assert!(menu.button("Обращения · ").is_none(), "{:?}", menu.body);
    assert!(menu.button("Обращения").is_some());
    // П-8 (картинка принята 18.11): копия обращения у владельца поправлена — кто ответил и что,
    // «Ответить» больше нет.
    let edited = o.edit_of(0, OWNER, owner_copy.message_id).await;
    let text = edited.text();
    assert!(
        text.contains("Ответил(а) <b>Анна</b>") && text.contains("деньги у нас"),
        "{text}"
    );
    assert!(text.contains("чек не пришёл"), "вопрос на месте: {text}");
    assert!(
        edited.button("Ответить").is_none(),
        "{:?}",
        edited.buttons()
    );
    // «Ответить» на старой копии: бот показывает ответ и спрашивает, клиенту — ничего.
    let mark = o.tg.mark();
    let again = o
        .press(ADMIN, &question, "Ответить", "уже ответил(а) <b>Анна</b>")
        .await;
    assert!(again.text().contains("деньги у нас"), "{}", again.text());
    let to_client =
        o.tg.calls()
            .into_iter()
            .skip(mark)
            .filter(|c| c.chat() == Some(4242))
            .count();
    assert_eq!(to_client, 0);
    o.press(ADMIN, &again, "Ответить ещё раз", "Ответ клиенту 4242")
        .await;
    o.say(ADMIN, "и ещё: чек уже ушёл", "Отправлено клиенту")
        .await;
    o.tg.screen(mark, 4242, "чек уже ушёл").await;
}

/// «Система» по макету: версия, база, очередь, Bot API, аккаунты; «Предпросмотр экранов»
/// присылает экраны клиента и пульта настоящими сообщениями.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn system_screen_and_screen_preview_from_the_panel(pool: PgPool) {
    let o = ops(pool).await;
    let menu = o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    let system = o.press(OWNER, &menu, "Система", "Очередь операций").await;
    assert_eq!(system.method, "editMessageText");
    let body = system.text();
    assert!(
        body.contains("Версия") && body.contains("Bot API"),
        "{body}"
    );
    let mark = o.tg.mark();
    o.press(
        OWNER,
        &system,
        "Предпросмотр экранов",
        "Предпросмотр отправлен",
    )
    .await;
    // Экраны идут по порядку макета, с паузой между сообщениями: проверяем начало.
    let ui = ui::Ui::builtin(ui::EmojiSet::plain(), ui::UiConfig::default()).unwrap();
    let total = ui::preview::client(&ui).len() + ui::preview::client_supplements(&ui).len();
    o.tg.screen(mark, OWNER, &format!("1/{total} · Приветствие"))
        .await;
    let exchange = o.tg.screen(mark, OWNER, "Что меняем?").await;
    assert_eq!(exchange.buttons(), vec!["a:x", "a:x"]);
}

/// Главный экран по макету («вариант А»): тишина → пожар после недостачи; карточка правится
/// сама в том же сообщении, красная кнопка — только сейчас.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn dash_turns_to_fire_on_a_shortfall_and_refreshes_itself(pool: PgPool) {
    let o = ops(pool).await;
    let calm = o.say(OWNER, "/dash", "Всё спокойно").await;
    assert_eq!(calm.method, "sendRichMessage", "главный экран — карточка");
    assert!(calm.button("Статистика").is_some(), "{:?}", calm.buttons());
    // В xRocket на 100 меньше, чем по леджеру: сверка ставит выплаты на паузу.
    o.world
        .set_balance(o.ub, XR, Asset::Usdt, dec!(900))
        .unwrap();
    o.engine.reconcile_balances().await.unwrap();
    let mark = o.tg.mark();
    o.bot.deliver_alerts().await.unwrap();
    let fire = o.tg.screen(mark, OWNER, "Недостача xRocket").await;
    assert_eq!(fire.method, "editMessageText", "правится, а не множится");
    assert_eq!(fire.message_id, calm.message_id);
    let body = fire.text();
    assert!(body.contains("Выплаты в xRocket на паузе"), "{body}");
    assert!(body.contains("Нужно решить · 1"), "{body}");
    assert!(body.contains("100.00"), "{body}");
    assert!(
        body.contains("Реально"),
        "в пожар — «Леджер · Реально»: {body}"
    );
    let shortfall = fire.body["reply_markup"]["inline_keyboard"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| r.as_array().cloned().unwrap_or_default())
        .find(|b| b["text"].as_str().is_some_and(|t| t.contains("Недостача")))
        .unwrap();
    assert_eq!(shortfall["style"], "danger", "{shortfall}");
}

/// П3 (макет «Главный экран пульта при паузах», принят 14.11): обслуживание, направление,
/// закрытое вручную, и отключённый аккаунт — жёлтые строки «Нужно решить», а не «Всё спокойно».
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn dash_is_not_calm_while_something_is_paused(pool: PgPool) {
    let o = ops(pool).await;
    o.say(OWNER, "/dash", "Всё спокойно").await;
    storage::settings::set(
        &mut o.pool.acquire().await.unwrap(),
        "maintenance",
        &serde_json::json!({"on": true}),
        None,
    )
    .await
    .unwrap();
    // Закрыто кнопкой пульта — в журнале «direction.close»; закрытые с начала не в счёт.
    sqlx::query("UPDATE directions SET is_open = false WHERE code = 'xr_to_cb_check'")
        .execute(&o.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO admin_audit (staff_id, action, target_type, target_id) \
         VALUES ($1, 'direction.close', 'direction', 'xr_to_cb_check')",
    )
    .bind(OWNER)
    .execute(&o.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO userbot_accounts (label, phone_masked, status) VALUES ('ub-2', '+7***00', 'disabled')",
    )
    .execute(&o.pool)
    .await
    .unwrap();
    let dash = o.say(OWNER, "/dash", "Нужно решить · 3").await;
    let body = dash.text();
    assert!(!body.contains("Всё спокойно"), "{body}");
    for row in [
        "Обслуживание",
        "Закрыто вручную",
        "xR → CB",
        "ub-2 отключён",
    ] {
        assert!(body.contains(row), "нет строки «{row}»: {body}");
    }
    assert!(body.contains("обслуживание"), "подвал: {body}");
    assert!(!body.contains("выплаты идут"), "подвал: {body}");
    assert!(dash.button("Настройки").is_some(), "{:?}", dash.buttons());
    assert!(
        dash.button("Направления · 1").is_some(),
        "{:?}",
        dash.buttons()
    );
    assert!(dash.button("Аккаунты").is_some(), "{:?}", dash.buttons());
}

/// Макет «Недостача»: в кошельке меньше, чем по леджеру, — «Это вы выводили?»; «Нет,
/// разбираться» оставляет выплаты на паузе и закрывает повтор события.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn shortfall_asks_the_owner_and_investigation_keeps_payouts_paused(pool: PgPool) {
    let o = ops(pool).await;
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    o.world
        .set_balance(o.ub, XR, Asset::Usdt, dec!(988))
        .unwrap();
    let mark = o.tg.mark();
    o.engine.reconcile_balances().await.unwrap();
    o.bot.deliver_alerts().await.unwrap();
    let alert = o.tg.screen(mark, OWNER, "меньше на").await;
    let body = alert.text();
    assert!(
        body.contains("xRocket: меньше на <code>12.00</code>"),
        "{body}"
    );
    assert!(body.contains("Выплаты в xRocket на паузе"), "{body}");
    assert!(body.contains("Это вы выводили?"), "{body}");
    let no = alert.body["reply_markup"]["inline_keyboard"][1][0].clone();
    assert_eq!(no["style"], "danger", "{no}");
    let after = o
        .press(OWNER, &alert, "Нет, разбираться", "Разбираемся")
        .await;
    assert!(
        after.text().contains("остаются на паузе"),
        "{}",
        after.text()
    );
    assert!(after.text().contains("ub-1"), "{}", after.text());
    // Событие закрыто — больше не повторяется; выплаты в xRocket всё ещё на паузе.
    let mut conn = o.pool.acquire().await.unwrap();
    let key = engine::reconcile::mismatch_key(XR);
    assert!(
        storage::alerts::open_by_key(&mut conn, &key)
            .await
            .unwrap()
            .is_none()
    );
    let row = storage::directions::get(&mut conn, Direction::CbToXrCheck)
        .await
        .unwrap();
    assert!(row.auto_paused, "выплаты в xRocket на паузе");
}

/// Решение владельца 8 ноября: номер в таблице — кнопка в тексте (callback, как в гаранте),
/// карточка открывается в том же сообщении; длинный список — страницами «1 / 2»; внизу
/// карточки — «‹ Заявки» обратно на ту же страницу.
/// Шаг 1.5 (утверждено 13.11): выплата неясна («выполнил и вернул таймаут»), сверка по балансу
/// видит, что она прошла, — карточка заявки в пульте подсказывает это оператору.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn order_card_shows_what_reconciliation_says_about_an_unknown_payout(pool: PgPool) {
    use userbot::sim::{Call, Fault};
    let o = ops(pool).await;
    let url = o.world.client_check(CB, usdt(dec!(100))).unwrap();
    let link = parsers::links::parse_url(&url).unwrap();
    let order = accepted(o.engine.submit_check(common::CLIENT, &link).await.unwrap());
    o.world.inject(
        o.ub,
        Call::CheckPress,
        Fault::DelayReply(Duration::from_millis(700)),
        1,
    );
    o.world
        .inject(o.ub, Call::History, Fault::DelayReply(Duration::ZERO), 2);
    o.world.inject(o.ub, Call::History, Fault::Disconnected, 3);
    o.drain().await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    o.world.clear_faults(o.ub);
    let r = o.engine.reconcile_balances().await.unwrap();
    assert!(
        r.iter()
            .all(|x| x.verdict == engine::reconcile::Verdict::Ok),
        "{r:?}"
    );
    let menu = o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    let orders = o.press(OWNER, &menu, "Заявки", "Заявки · 1").await;
    let card = o.follow(OWNER, &orders, &order.public_id, "Заявка").await;
    let text = card.text();
    assert!(
        text.contains("по балансу xRocket выплата") && text.contains("прошла"),
        "{text}"
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn table_rows_open_cards_in_the_same_window_and_lists_have_pages(pool: PgPool) {
    let o = ops(pool).await;
    for i in 0..10 {
        let url = o
            .world
            .client_check(CB, usdt(domain::Decimal::from(10 + i)))
            .unwrap();
        let link = parsers::links::parse_url(&url).unwrap();
        accepted(o.engine.submit_check(common::CLIENT, &link).await.unwrap());
    }
    let menu = o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    let orders = o.press(OWNER, &menu, "Заявки", "Заявки · 10").await;
    assert!(orders.text().contains("1 / 2"), "{}", orders.text());
    assert!(orders.text().contains("Нажмите номер"), "{}", orders.text());
    let page2 = o.press(OWNER, &orders, "Дальше", "2 / 2").await;
    assert_eq!(
        page2.message_id, orders.message_id,
        "листание — в том же окне"
    );
    // Самая старая заявка — на второй странице.
    let (oldest,): (String,) = sqlx::query_as("SELECT public_id FROM orders ORDER BY id LIMIT 1")
        .fetch_one(&o.pool)
        .await
        .unwrap();
    let mark = o.tg.mark();
    let card = o.follow(OWNER, &page2, &oldest, "Заявка").await;
    assert_eq!(card.method, "editMessageText");
    assert_eq!(
        card.message_id, page2.message_id,
        "карточка — в том же сообщении"
    );
    // Только окно сотрудника: клиентский бот в это время может слать клиенту сообщения его
    // заявок («Проверяю чек») — это другой чат, и CI ловил их здесь как ложную ошибку.
    let sent: Vec<String> =
        o.tg.calls()
            .iter()
            .skip(mark)
            .filter(|c| c.chat() == Some(OWNER))
            .filter(|c| c.method == "sendMessage" || c.method == "sendRichMessage")
            .map(|c| format!("{} → {:?}: {}", c.method, c.chat(), c.text()))
            .collect();
    assert!(
        sent.is_empty(),
        "никаких новых сообщений и «/start» — нажатие приходит кнопкой: {sent:#?}"
    );
    let back = o.press(OWNER, &card, "Заявки", "2 / 2").await;
    assert!(back.text().contains(&oldest), "назад — на ту же страницу");
}

/// «Ребалансировка» карточкой (шаг 2): рассылку «🔥 Выгодно» включает только владелец — с
/// подтверждением «было → станет»; админ видит её состояние, но кнопки нет.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn rebalance_card_broadcast_switch_is_owner_only(pool: PgPool) {
    let o = ops(pool).await;
    let card = o.say(OWNER, "/promo", "Ребалансировка").await;
    assert_eq!(card.method, "sendRichMessage", "карточка");
    assert!(
        card.text().contains("Рассылка клиентам: выключена"),
        "{}",
        card.text()
    );
    let preview = o
        .press(OWNER, &card, "Включить рассылку", "не чаще раза в 3 часа")
        .await;
    assert!(preview.text().contains("выкл"), "{}", preview.text());
    o.press(
        OWNER,
        &preview,
        "Подтвердить",
        "Рассылка клиентам: включена",
    )
    .await;
    let admin = o.say(ADMIN, "/promo", "Ребалансировка").await;
    assert!(
        admin.text().contains("Рассылка клиентам: включена"),
        "{}",
        admin.text()
    );
    assert!(
        admin.button("Выключить рассылку").is_none(),
        "{:?}",
        admin.buttons()
    );
}

/// Заявка уходит на разбор: в кошельке xRocket 5 USDT вместо 1000 — выплата не удалась.
async fn order_in_review(o: &Ops) -> i64 {
    o.world.set_balance(o.ub, XR, Asset::Usdt, dec!(5)).unwrap();
    let url = o.world.client_check(CB, usdt(dec!(100))).unwrap();
    let link = parsers::links::parse_url(&url).unwrap();
    let order = accepted(o.engine.submit_check(common::CLIENT, &link).await.unwrap());
    o.drain().await;
    order.id
}

/// Шаг 2.8 (макет «Алерт и «Принял»», выбор владельца 14.11): критичную тревогу никто не принял —
/// раз в `repeat_every` она приходит новым сообщением со звуком, прежнее удаляется. «Принял» у
/// одного — у всех правка «Принял … — повторы остановлены», и тревога не возвращается, пока
/// заявка на разборе (раньше следующий круг через 5 с открывал её заново — AUDIT-UI П1).
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_critical_alert_repeats_until_acked_and_the_ack_silences_it(pool: PgPool) {
    let o = ops_with(pool, |c| c.repeat_every = Duration::from_millis(500)).await;
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    o.say(ADMIN, "/menu", "<b>Пульт</b>").await;
    let mark = o.tg.mark();
    order_in_review(&o).await;
    let first = o.tg.screen(mark, OWNER, "Нужен разбор").await;
    assert_eq!(first.method, "sendMessage");
    assert_eq!(
        first.body["disable_notification"],
        json!(false),
        "critical — with sound"
    );

    let mark = o.tg.mark();
    let again = o.tg.screen(mark, OWNER, "Повтор 1").await;
    assert_eq!(again.method, "sendMessage", "a repeat is a new message");
    assert_ne!(again.message_id, first.message_id);
    // До 5 секунд: под нагрузкой полного прогона удаление прежнего приходит не сразу.
    for _ in 0..500 {
        if o.tg.calls().iter().skip(mark).any(|c| {
            c.method == "deleteMessage"
                && c.chat() == Some(OWNER)
                && c.body["message_id"].as_i64() == first.message_id
        }) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        o.tg.calls()
            .iter()
            .skip(mark)
            .any(|c| c.method == "deleteMessage"
                && c.body["message_id"].as_i64() == first.message_id),
        "the previous copy is deleted: one alert in the chat"
    );

    let mark = o.tg.mark();
    let acked = o
        .press(OWNER, &again, "Принял", "повторы остановлены")
        .await;
    assert_eq!(acked.method, "editMessageText");
    assert!(acked.text().contains("Игнат"), "{}", acked.text());
    assert!(acked.button("Принял").is_none());
    o.tg.screen(mark, ADMIN, "повторы остановлены").await;

    // Заявка всё ещё на разборе, но тревога не возвращается: ни повтора, ни новой.
    let mark = o.tg.mark();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let back: Vec<String> =
        o.tg.calls()
            .iter()
            .skip(mark)
            .filter(|c| c.method == "sendMessage" && c.text().contains("Нужен разбор"))
            .map(Call::text)
            .collect();
    assert!(back.is_empty(), "acked alert came back: {back:#?}");
}

/// Шаг 2.8: копии тревоги — в базе. Пульт перезапущен, «Принял» правит сообщение, отправленное
/// до перезапуска, а не шлёт новое.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn after_a_restart_the_ack_edits_the_copy_sent_before_it(pool: PgPool) {
    let o = ops(pool).await;
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    let mark = o.tg.mark();
    order_in_review(&o).await;
    let alert = o.tg.screen(mark, OWNER, "Нужен разбор").await;
    // Перезапуск пульта: память о копиях пропала, база — нет.
    o.cancel.cancel();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let bot = OpsBot::new(
        o.tg.api(),
        o.engine.clone(),
        o.pool.clone(),
        OpsConfig {
            alert_every: Duration::from_millis(50),
            ..OpsConfig::default()
        },
    )
    .unwrap();
    let cancel = CancellationToken::new();
    {
        let bot = bot.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { bot.run(cancel).await });
    }
    let alert_id: i64 = sqlx::query_scalar("SELECT id FROM alerts WHERE title = 'manual_review'")
        .fetch_one(&o.pool)
        .await
        .unwrap();
    let mark = o.tg.mark();
    let press: botapi::Update = serde_json::from_value(json!({
        "update_id": 999_001,
        "callback_query": {
            "id": "cb-restart",
            "from": { "id": OWNER, "is_bot": false, "first_name": "Staff" },
            "message": { "message_id": alert.message_id, "date": 1, "chat": { "id": OWNER, "type": "private" } },
            "chat_instance": "1",
            "data": ops::Cb::Ack(alert_id).encode(),
        }
    }))
    .unwrap();
    assert!(tgbot::UpdateHook::try_handle(&bot, &press).await);
    let edited = o.tg.screen(mark, OWNER, "повторы остановлены").await;
    assert_eq!(edited.method, "editMessageText");
    assert_eq!(
        edited.message_id, alert.message_id,
        "the copy sent before the restart"
    );
    cancel.cancel();
}

/// Б-1 (аудит 11.11): номер заявки в обращении приходит из кнопки клиента. Чужая заявка (номер
/// увиден на скриншоте) к вопросу не прикладывается — оператор не ответит ссылкой на чужой чек.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_support_question_never_carries_someone_elses_order(pool: PgPool) {
    use tgbot::UpdateHook as _;
    let o = ops(pool).await;
    // Заявка клиента CLIENT; спрашивает другой человек.
    let order = order_in_review(&o).await;
    let public: String = sqlx::query_scalar("SELECT public_id FROM orders WHERE id = $1")
        .bind(order)
        .fetch_one(&o.pool)
        .await
        .unwrap();
    let stranger = botapi::User {
        id: 777_001,
        is_bot: false,
        first_name: "Чужой".into(),
        last_name: None,
        username: None,
        language_code: None,
        is_premium: None,
    };
    let mark = o.tg.mark();
    assert!(
        o.bot
            .support_question(&stranger, "где мои деньги?", Some(&public))
            .await
    );
    let question = o.tg.screen(mark, OWNER, "Обращение").await;
    assert!(
        question.text().contains("где мои деньги?"),
        "{}",
        question.text()
    );
    assert!(
        !question.text().contains(&public),
        "чужая заявка не приложена: {}",
        question.text()
    );
    assert!(question.button("Заявка").is_none(), "{:?}", question.body);
}

/// Д-19.1 (проверка 19.11, картинка «Застрявшие переплата и возврат» принята 20.11). Чек на
/// переплату по оплаченному счёту не создался: тревога «Нужен разбор» со строкой «Переплата»,
/// карточка выполненной заявки с «Повторить чек переплаты» и «Чек выдан»; «Чек выдан» —
/// причина, ссылка на чек кошелька переплаты (чужой — отказ), подтверждение; клиент получает
/// «Забрать», заявка уходит из «Разбора».
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn stuck_overpayment_is_closed_from_the_console_with_the_found_check(pool: PgPool) {
    use userbot::sim::{Call as SimCall, CheckSpec, Fault, InvoiceSpec};
    let o = ops(pool).await;
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    {
        let mut conn = o.pool.acquire().await.unwrap();
        for d in [Direction::PayCbInvoice, Direction::PayXrInvoice] {
            storage::directions::set_open(&mut conn, d, true)
                .await
                .unwrap();
        }
    }
    let shop = o.world.add_account("shop");
    let inv = o
        .world
        .create_invoice(InvoiceSpec::crypto(CB, usdt(dec!(10))).by(shop))
        .unwrap();
    let link = parsers::links::parse_url(&inv.url).unwrap();
    let order = match o
        .engine
        .submit_invoice(common::CLIENT, &link)
        .await
        .unwrap()
    {
        engine::InvoiceSubmitted::Quoted(order) => order,
        other => panic!("{other:?}"),
    };
    let check = o.world.client_check(XR, usdt(dec!(15))).unwrap();
    let check = parsers::links::parse_url(&check).unwrap();
    accepted(o.engine.submit_check(common::CLIENT, &check).await.unwrap());
    // «✅ Подтверждаю» чека на переплату не дошло: исход неясен, чек не создан.
    o.world
        .inject(o.ub, SimCall::CheckPress, Fault::TimeoutBeforeEffect, 1);
    let mark = o.tg.mark();
    o.drain().await;
    o.engine.resolve_unknown().await.unwrap();
    let alert = o.tg.screen(mark, OWNER, "Нужен разбор").await;
    let text = alert.text();
    assert!(text.contains(&order.public_id), "{text}");
    assert!(text.contains("Переплата <code>4.70</code>"), "{text}");
    assert!(
        text.contains("<code>15.00</code> ") && text.contains(" → <code>10.00</code> счёт "),
        "{text}"
    );

    let card = o
        .press(OWNER, &alert, "Открыть", "Переплата не вернулась")
        .await;
    // Состояние — словом утверждённых экранов пульта («Готово»), не «Выполнена» с картинки.
    assert!(card.text().contains("Готово"), "{}", card.text());
    assert!(
        card.button("Повторить чек переплаты").is_some(),
        "{:?}",
        card.buttons()
    );
    let ask = o.press(OWNER, &card, "Чек выдан", "Коротко причина").await;
    assert!(ask.text().contains(&order.public_id), "{}", ask.text());
    let prompt = o
        .say(
            OWNER,
            "нашёл чек в «Мои чеки» xRocket",
            "Пришлите ссылку на чек xRocket",
        )
        .await;
    assert!(prompt.text().contains("4.70"), "{}", prompt.text());
    o.say(
        OWNER,
        "https://t.me/send?start=CQAbCdEfGhIj",
        "ссылка на чек xRocket",
    )
    .await;
    let found = o
        .world
        .create_check(CheckSpec::new(XR, usdt(dec!(4.70))).by(o.ub))
        .unwrap();
    let preview = o
        .say(OWNER, &found.url, "Отметить чек переплаты выданным?")
        .await;
    assert!(
        preview.text().contains("<tg-spoiler>"),
        "{}",
        preview.text()
    );
    let done = o
        .press(OWNER, &preview, "Чек выдан", "<b>Чек переплаты выдан</b>")
        .await;
    assert!(done.text().contains(&order.public_id), "{}", done.text());

    let mut conn = o.pool.acquire().await.unwrap();
    let (_, d) = (
        storage::orders::get(&mut conn, order.id, false)
            .await
            .unwrap(),
        storage::orders::details(&mut conn, order.id).await.unwrap(),
    );
    assert_eq!(d.payout_check_url.as_deref(), Some(found.url.as_str()));
    assert!(
        storage::admin::review_queue(&mut conn)
            .await
            .unwrap()
            .iter()
            .all(|q| q.id != order.id)
    );
    let audit = storage::audit::recent(&mut conn, 10, None, Some(("order", &order.id.to_string())))
        .await
        .unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "order.excess_issued" && a.staff_id == OWNER)
    );
}

/// Д-19.3 (картинка last-fixes.png принята 20.11): подтверждение «Отметить выплаченной» по
/// счёту говорит, что будет с переплатой, — вернётся чеком или останется в доходе. Красная
/// кнопка — отдельной строкой: рядом с «Отмена» надпись обрезалась до «Отметить вып…».
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn marking_an_invoice_paid_says_what_happens_to_the_overpayment(pool: PgPool) {
    use userbot::sim::{Call as SimCall, Fault, InvoiceSpec};
    let o = ops(pool).await;
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    {
        let mut conn = o.pool.acquire().await.unwrap();
        for d in [Direction::PayCbInvoice, Direction::PayXrInvoice] {
            storage::directions::set_open(&mut conn, d, true)
                .await
                .unwrap();
        }
    }
    let shop = o.world.add_account("shop");
    let inv = o
        .world
        .create_invoice(InvoiceSpec::crypto(CB, usdt(dec!(10))).by(shop))
        .unwrap();
    let link = parsers::links::parse_url(&inv.url).unwrap();
    let order = match o
        .engine
        .submit_invoice(common::CLIENT, &link)
        .await
        .unwrap()
    {
        engine::InvoiceSubmitted::Quoted(order) => order,
        other => panic!("{other:?}"),
    };
    let check = o.world.client_check(XR, usdt(dec!(15))).unwrap();
    let check = parsers::links::parse_url(&check).unwrap();
    accepted(o.engine.submit_check(common::CLIENT, &check).await.unwrap());
    // «Оплатить» сработало, но бот молчит — заявка у человека.
    o.world
        .inject(o.ub, SimCall::Press, Fault::DelayReply(Duration::ZERO), 1);
    o.world.inject(o.ub, SimCall::Press, Fault::NoReply, 1);
    let mark = o.tg.mark();
    o.drain().await;
    let alert = o.tg.screen(mark, OWNER, "Нужен разбор").await;
    let card = o.press(OWNER, &alert, "Открыть", "Ручной разбор").await;
    o.press(OWNER, &card, "Выплачено", "Коротко причина").await;
    o.say(OWNER, "счёт оплачен вручную", "Доказательство выплаты")
        .await;
    let preview = o
        .say(
            OWNER,
            "оплатил кнопкой в @send, 14:10",
            "Отметить выплаченной?",
        )
        .await;
    let text = preview.text();
    assert!(text.contains(&order.public_id), "{text}");
    assert!(
        text.contains("Переплата <code>4.70</code> USDT вернётся клиенту чеком xRocket."),
        "{text}"
    );
    let rows = preview.body["reply_markup"]["inline_keyboard"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let confirm_row = rows
        .iter()
        .find(|r| r.to_string().contains("Отметить выплаченной"))
        .expect("кнопка подтверждения");
    assert_eq!(
        confirm_row.as_array().map(Vec::len),
        Some(1),
        "красная кнопка — отдельной строкой: {rows:?}"
    );
}

/// Д-19.11: неясный возврат на разборе — «Вернуть» и «Возврат выдан» вместо «Повторить выплату»
/// и «Выплачено»; «Возврат выдан» со ссылкой закрывает заявку возвратом.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn unclear_refund_is_closed_from_the_console_with_the_found_check(pool: PgPool) {
    use userbot::sim::{Call as SimCall, CheckSpec, Fault};
    let o = ops(pool).await;
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    let url = o.world.client_check(CB, usdt(dec!(1))).unwrap();
    let link = parsers::links::parse_url(&url).unwrap();
    let order = accepted(o.engine.submit_check(common::CLIENT, &link).await.unwrap());
    o.world.inject(
        o.ub,
        SimCall::SendText,
        Fault::DelayReply(Duration::ZERO),
        3,
    );
    o.world.inject(o.ub, SimCall::SendText, Fault::NoReply, 1);
    let mark = o.tg.mark();
    o.drain().await;
    let alert = o.tg.screen(mark, OWNER, "Нужен разбор").await;
    let card = o.press(OWNER, &alert, "Открыть", "Ручной разбор").await;
    assert!(
        card.button("Возврат выдан").is_some(),
        "{:?}",
        card.buttons()
    );
    assert!(card.button("Вернуть").is_some(), "{:?}", card.buttons());
    assert!(
        card.button("Повторить выплату").is_none(),
        "{:?}",
        card.buttons()
    );
    assert!(card.button("Выплачено").is_none(), "{:?}", card.buttons());
    o.press(OWNER, &card, "Возврат выдан", "Коротко причина")
        .await;
    o.say(
        OWNER,
        "чек есть в «Мои чеки»",
        "Пришлите ссылку на чек CryptoBot",
    )
    .await;
    let found = o
        .world
        .create_check(CheckSpec::new(CB, usdt(dec!(1))).by(o.ub))
        .unwrap();
    let preview = o.say(OWNER, &found.url, "Отметить возврат выданным?").await;
    o.press(OWNER, &preview, "Возврат выдан", "<b>Возврат выдан</b>")
        .await;
    let mut conn = o.pool.acquire().await.unwrap();
    let after = storage::orders::get(&mut conn, order.id, false)
        .await
        .unwrap();
    assert_eq!(after.state, OrderState::Refunded);
}

/// С-19.1 (проверка 19.11): первая отправка тревоги не дошла ни до кого (нет связи с Telegram).
/// Раньше тревога считалась доставленной и больше не уходила, а повтор критичных считался от
/// копий, которых не было, — «Нужен разбор» не приходил никогда. Теперь она уходит, как только
/// Telegram снова отвечает.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn an_alert_that_could_not_be_sent_goes_out_when_telegram_answers_again(pool: PgPool) {
    let o = ops(pool).await;
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    o.say(ADMIN, "/menu", "<b>Пульт</b>").await;
    // Пульт молчит: тревоги разносит только явный круг ниже.
    o.cancel.cancel();
    tokio::time::sleep(Duration::from_millis(100)).await;
    o.tg.lock().fail_sends = 1_000;
    order_in_review(&o).await;
    o.bot.deliver_alerts().await.unwrap();
    let mark = o.tg.mark();
    o.tg.lock().fail_sends = 0;
    o.bot.deliver_alerts().await.unwrap();
    o.tg.screen(mark, OWNER, "Нужен разбор").await;
    o.tg.screen(mark, ADMIN, "Нужен разбор").await;
}

/// Т-5 (картинка console-login.png принята 20.11): «Вход сброшен» → одна кнопка «Как войти» —
/// шаги на сервере карточкой. Раньше «Войти заново» и «Заменить» вели в «Аккаунты», где войти
/// нельзя: тупик.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_lost_login_alert_shows_how_to_log_in_again(pool: PgPool) {
    let o = ops(pool).await;
    o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    {
        let mut conn = o.pool.acquire().await.unwrap();
        let ub: i16 = sqlx::query_scalar("SELECT id FROM userbot_accounts WHERE label = 'ub-1'")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        storage::alerts::raise(
            &mut conn,
            &storage::alerts::NewAlert {
                level: storage::alerts::Level::Critical,
                topic: storage::alerts::Topic::Errors,
                dedup_key: "login_lost:ub-1",
                title: "login_lost",
                payload: json!({"account": "ub-1", "userbot_id": ub, "platforms": ["xrocket"]}),
            },
        )
        .await
        .unwrap();
    }
    let mark = o.tg.mark();
    o.bot.deliver_alerts().await.unwrap();
    let alert = o.tg.screen(mark, OWNER, "вход сброшен").await;
    let labels = alert.labels();
    assert!(
        labels
            .iter()
            .all(|b| !b.contains("Войти заново") && !b.contains("Заменить")),
        "тупиковых кнопок нет: {labels:?}"
    );
    let how = o
        .press(OWNER, &alert, "Как войти", "exch service stop")
        .await;
    let text = how.text();
    for needle in [
        "Как войти · ub-1",
        "Завершить все",
        "exch setup",
        "exch service start",
        "Номер потерян",
    ] {
        assert!(text.contains(needle), "{needle:?} в карточке: {text}");
    }
    let buttons = how.labels();
    assert!(
        buttons.iter().any(|b| b.contains("Аккаунты"))
            && buttons.iter().any(|b| b.contains("Меню")),
        "{buttons:?}"
    );
}

/// П13: на примерах пульта из «Предпросмотра» ни одна кнопка не ведёт к настоящему действию —
/// весь присланный «Предпросмотр», не только `sample()`: у примеров тревог настоящие номера
/// аккаунтов и направлений («Отключить» на «Вход сброшен» — аккаунт №2).
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn panel_preview_samples_carry_no_live_buttons(pool: PgPool) {
    let o = ops(pool).await;
    let mark = o.tg.mark();
    ops::preview::send_panel_preview(&o.tg.api(), &ops::Look::plain(), OWNER).await;
    let sample = ops::Cb::Sample.encode();
    let live: Vec<String> =
        o.tg.calls()
            .into_iter()
            .skip(mark)
            .flat_map(|c| {
                let head: String = c.text().chars().take(60).collect();
                c.buttons()
                    .into_iter()
                    .filter(|d| *d != sample)
                    .map(move |d| format!("{d} · {head}"))
            })
            .collect();
    assert!(live.is_empty(), "живые кнопки на примерах: {live:#?}");
}

/// Решение владельца 21.11: «ub-1: войти» на главном экране — тоже в «Как войти», а не в
/// «Аккаунты», где войти нельзя.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn the_dash_login_button_shows_how_to_log_in_again(pool: PgPool) {
    let o = ops(pool).await;
    {
        let mut conn = o.pool.acquire().await.unwrap();
        storage::sessions::mark_logged_out(&mut conn, "ub-1")
            .await
            .unwrap();
    }
    let dash = o.say(OWNER, "/dash", "вход сброшен").await;
    let how = o
        .press(OWNER, &dash, "ub-1: войти", "exch service stop")
        .await;
    assert!(how.text().contains("Как войти · ub-1"), "{}", how.text());
}

/// Возврат с каждого экрана пульта (решение владельца 21.11, SCREENS-21-11, раздел 1): бот
/// нажимает каждую кнопку каждого экрана от меню; на каждом экране, кроме меню, есть путь назад,
/// а домой ведёт одна кнопка — «Меню». Сегодняшние нарушения — `BAD_BACK`, список только
/// сокращается.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn every_panel_screen_has_a_way_back(pool: PgPool) {
    /// Кусок текста экрана, у которого нарушение известно (исправить — и убрать строку).
    const BAD_BACK: &[&str] = &[
        // «Отмена» ведёт в «Пульт», а не на экран, где нажали.
        "Отменено. Ничего не изменено.",
        // Итог самопроверки: «Главный экран» и «Пульт» вместо экрана, откуда пришли.
        "Самопроверка",
    ];
    let o = ops(pool).await;
    let first_line = |c: &Call| -> String {
        let body = c.body["rich_message"]["html"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| c.text());
        let mut text = String::new();
        let mut tag = false;
        for ch in body
            .replace("<br/>", "\n")
            .replace("</p>", "\n")
            .replace("</h3>", "\n")
            .chars()
        {
            match ch {
                '<' => tag = true,
                '>' if tag => tag = false,
                c if !tag => text.push(c),
                _ => {}
            }
        }
        text.lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .to_owned()
    };
    let mut seen: std::collections::HashSet<String> = Default::default();
    let mut queue: std::collections::VecDeque<Vec<String>> = Default::default();
    let mut problems: Vec<String> = Vec::new();
    let mut found: std::collections::BTreeSet<String> = Default::default();
    queue.push_back(Vec::new());
    while let Some(path) = queue.pop_front() {
        if path.len() > 4 {
            continue;
        }
        let mut screen = o.say(OWNER, "/menu", "<b>Пульт</b>").await;
        let mut reached = true;
        for data in &path {
            let mark = o.tg.mark();
            o.tg.presses(OWNER, screen.message_id.unwrap_or(1), data);
            let next = tokio::time::timeout(Duration::from_millis(800), async {
                loop {
                    let calls: Vec<Call> = o.tg.calls().into_iter().skip(mark).collect();
                    let mine = |c: &&Call| c.chat() == Some(OWNER);
                    // Экран, на котором нажали (правка), важнее тревоги, пришедшей новым
                    // сообщением в тот же момент.
                    let edit = calls
                        .iter()
                        .filter(mine)
                        .rev()
                        .find(|c| c.method.starts_with("editMessage"));
                    let sent =
                        calls.iter().filter(mine).rev().find(|c| {
                            matches!(c.method.as_str(), "sendMessage" | "sendRichMessage")
                        });
                    if let Some(c) = edit.or(sent) {
                        return c.clone();
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
            match next {
                Ok(c) => screen = c,
                Err(_) => {
                    reached = false;
                    break;
                }
            }
        }
        if !reached {
            continue;
        }
        let labels = screen.labels();
        let head = first_line(&screen);
        let text = screen.body["rich_message"]["html"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| screen.text());
        if !path.is_empty() && !head.starts_with("Пульт") {
            let back = labels
                .iter()
                .any(|l| l.contains('◀') || l.ends_with("Меню") || l.contains("Отмена"));
            let odd_home = labels
                .iter()
                .any(|l| l.ends_with("Пульт") || l.contains("Главный экран") || l == "‹ Назад");
            if !back || odd_home {
                let known = BAD_BACK.iter().any(|k| text.contains(k));
                found.insert(text.clone());
                if !known {
                    problems.push(format!(
                        "«{head}»: кнопки {labels:?} — нет возврата или домой не «Меню»"
                    ));
                }
            }
        }
        let mut datas: Vec<String> = screen.buttons();
        let body = screen.body["rich_message"]["html"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        for part in body.split("data=\"").skip(1) {
            if let Some(d) = part.split('"').next() {
                datas.push(d.to_owned());
            }
        }
        for d in datas {
            if d == "a:x" || d == "a:pv" || !seen.insert(d.clone()) {
                continue;
            }
            let mut p = path.clone();
            p.push(d);
            queue.push_back(p);
        }
    }
    for k in BAD_BACK {
        if !found.iter().any(|h| h.contains(k)) {
            problems.push(format!("«{k}»: уже в порядке — уберите из BAD_BACK"));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}
