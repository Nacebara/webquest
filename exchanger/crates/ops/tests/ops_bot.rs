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
    let bot = OpsBot::new(
        tg.api(),
        engine.clone(),
        pool.clone(),
        OpsConfig {
            alert_every: Duration::from_millis(50),
            ..OpsConfig::default()
        },
    )
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
async fn fee_change_needs_a_preview_and_a_single_use_confirmation(pool: PgPool) {
    let o = ops(pool).await;
    let panel = o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    let dirs = o
        .press(OWNER, &panel, "Направления", "Комиссия, ступени")
        .await;
    let card = o
        .follow(OWNER, &dirs, "CryptoBot → xRocket", "Рычаг резерва")
        .await;
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
    let panel = o.say(ADMIN, "/dir", "Комиссия, ступени").await;
    let card = o
        .follow(ADMIN, &panel, "xRocket → CryptoBot", "Рычаг резерва")
        .await;
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
    let card = o
        .follow(ADMIN, &panel, "xRocket → CryptoBot", "Рычаг резерва")
        .await;
    o.press(ADMIN, &card, "Комиссия", "Новая комиссия").await;
    // Владелец поставил 4 %: 3.5 % — сдвиг на 0.5 п.п.
    let preview = o.say(ADMIN, "3.5", "проверьте изменение").await;
    o.press(ADMIN, &preview, "Подтвердить", "Сохранено").await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn tiers_and_switch_to_flat_and_back(pool: PgPool) {
    let o = ops(pool).await;
    let panel = o.say(OWNER, "/dir", "Комиссия, ступени").await;
    let card = o
        .follow(OWNER, &panel, "CryptoBot → xRocket", "Рычаг резерва")
        .await;
    // Режим ступеней без ступеней — нельзя.
    o.press(
        OWNER,
        &card,
        "Перейти на ступени",
        "Сначала задайте ступени",
    )
    .await;
    let card = o
        .follow(OWNER, &panel, "CryptoBot → xRocket", "Рычаг резерва")
        .await;
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
        .press(OWNER, &settings, "Включить стоп-кран", "Стоп-кран выплат")
        .await;
    assert!(preview.text().contains("random_id"));
    o.press(OWNER, &preview, "Подтвердить", "Сохранено").await;
    assert!(
        storage::settings::flag(&mut conn, "kill_switch_payouts", false)
            .await
            .unwrap()
    );
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
    assert!(alert.text().contains(&format!("№{}", order.id)));
    // Админ тоже получил событие.
    o.tg.screen(mark, ADMIN, "Нужен разбор").await;
    o.world
        .set_balance(o.ub, XR, Asset::Usdt, dec!(1000))
        .unwrap();
    let card = o
        .press(OWNER, &alert, "Открыть заявку", "Ручной разбор")
        .await;
    // Карточка по макету: деньги клиента проведены — «Принято» без «не проведено».
    assert!(card.text().contains("Принято"), "{}", card.text());
    assert!(!card.text().contains("не проведено"), "{}", card.text());
    assert!(card.text().contains("Ход · "), "{}", card.text());
    o.press(OWNER, &card, "Вернуть", "Коротко причина").await;
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
    let done = o.press(OWNER, &preview, "Вернуть", "возврат").await;
    assert!(done.text().contains(&format!("Заявка №{}", order.id)));
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

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn selftest_from_the_panel_reports_every_step(pool: PgPool) {
    let o = ops(pool).await;
    // Макет убрал самопроверку с главного экрана: она — в «Аккаунтах».
    let dash = o.say(OWNER, "/dash", "Всё спокойно").await;
    let bots = o.press(OWNER, &dash, "Аккаунты", "Юзерботы").await;
    let preview = o
        .press(OWNER, &bots, "Самопроверка", "сам же его активирует")
        .await;
    let report = o
        .press(OWNER, &preview, "Подтвердить", "Всё работает")
        .await;
    assert!(!report.text().contains("❌"), "{}", report.text());
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
    assert!(
        dirs.text().contains("работает · заявка до"),
        "{}",
        dirs.text()
    );

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
    o.tg.screen(mark, OWNER, "Обращение").await;
    // В меню — счётчик и синяя кнопка: ждут человека (макет «Меню пульта»).
    let menu = o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    assert!(menu.button("Обращения · 1").is_some(), "{:?}", menu.body);
    let inbox = o
        .press(OWNER, &menu, "Обращения · 1", "Обращения · 1")
        .await;
    assert!(inbox.text().contains("4242"), "{}", inbox.text());

    o.press(ADMIN, &question, "Ответить", "Ответ клиенту 4242")
        .await;
    o.say(
        ADMIN,
        "деньги у нас, чек будет через 5 минут",
        "Отправлено клиенту · Анна",
    )
    .await;
    let answer = o.tg.screen(mark, 4242, "Оператор Анна:").await;
    assert!(answer.text().contains("деньги у нас"), "{}", answer.text());
    // Ответили — обращение закрыто, счётчика больше нет.
    let menu = o.say(OWNER, "/menu", "<b>Пульт</b>").await;
    assert!(menu.button("Обращения · ").is_none(), "{:?}", menu.body);
    assert!(menu.button("Обращения").is_some());
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
    o.tg.screen(mark, OWNER, "1/32 · Приветствие").await;
    o.tg.screen(mark, OWNER, "Что меняем?").await;
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
    assert!(
        o.tg.calls()
            .iter()
            .skip(mark)
            .all(|c| c.method != "sendMessage" && c.method != "sendRichMessage"),
        "никаких новых сообщений и «/start» — нажатие приходит кнопкой"
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
