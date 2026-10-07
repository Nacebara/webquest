//! `GrammersTransport` против поддельного сервера Telegram на 127.0.0.1: настоящий grammers 0.10,
//! настоящее шифрование MTProto 2.0 и сериализация TL, но логика сервера — из теста.
//!
//! Проверяется то, что важно для денег (LOVEC-PORTING §7): ровно тот `random_id`, что дал
//! вызывающий; «выполнил и не ответил» → исход неизвестен, повтор тем же `random_id` даёт
//! `RANDOM_ID_DUPLICATE`; нажатие кнопки не повторяется после обрыва; FLOOD_WAIT не «досыпается»
//! молча; сверка ботов при старте; поток апдейтов и переподключение.
//!
//! Это НЕ проверка поведения настоящих серверов Telegram и ботов кошельков — см. документацию
//! крейта, раздел «Что проверено, а что нет».

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use domain::Platform;
use grammers_tl_types as tl;
use mtproto::session::{EncryptedSession, SessionStore};
use mtproto::{ConnectError, GrammersTransport};
use support::fake_tg::{FakeTelegram, Reply, Request};
use support::*;
use tokio::sync::broadcast;
use userbot::transport::{ButtonKind, Chat, RawMessage, Transport, TransportError};

async fn fake(mut handler: impl FnMut(&Request) -> Option<Reply> + Send + 'static) -> FakeTelegram {
    FakeTelegram::start(move |req| {
        handler(req)
            .or_else(|| bootstrap(req))
            .unwrap_or_else(|| Reply::error(400, "METHOD_NOT_EXPECTED_IN_TEST"))
    })
    .await
}

async fn connect(fake: &FakeTelegram) -> GrammersTransport {
    let transport = GrammersTransport::connect(config(), key(), store_for(fake, true))
        .await
        .expect("connect to fake telegram");
    // Поток апдейтов получил состояние — пуши с pts > START_PTS применятся без разрыва.
    fake.wait_for("updates.getState", |r| {
        r.iter()
            .any(Request::is::<tl::functions::updates::GetState>)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    transport
}

async fn next(rx: &mut broadcast::Receiver<RawMessage>) -> RawMessage {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("no message from the update stream")
        .expect("update stream closed")
}

fn short_sent(id: i32) -> tl::enums::Updates {
    tl::types::UpdateShortSentMessage {
        out: true,
        id,
        pts: START_PTS + 50,
        pts_count: 1,
        date: now(),
        media: None,
        entities: None,
        ttl_period: None,
    }
    .into()
}

#[tokio::test]
async fn connect_verifies_bots_by_cached_hash() {
    let fake = fake(|_| None).await;
    let transport = connect(&fake).await;
    assert_eq!(transport.self_id(), SELF_ID);
    let bots = transport.bot_peers();
    assert_eq!(
        (bots.cryptobot.id, bots.cryptobot.access_hash),
        (CB, CB_HASH)
    );
    assert_eq!((bots.xrocket.id, bots.xrocket.access_hash), (XR, XR_HASH));
    assert_eq!(
        fake.count::<tl::functions::contacts::ResolveUsername>(),
        0,
        "known bots are checked with users.getUsers, not resolved"
    );
    transport.shutdown().await;
}

#[tokio::test]
async fn connect_resolves_unknown_bots_by_configured_username_and_saves_them() {
    let fake = fake(|_| None).await;
    let store = store_for(&fake, false);
    let transport = GrammersTransport::connect(config(), key(), Arc::clone(&store) as _)
        .await
        .unwrap();
    let resolved: Vec<String> = fake
        .parsed::<tl::functions::contacts::ResolveUsername>()
        .into_iter()
        .map(|r| r.username)
        .collect();
    assert_eq!(resolved, vec!["send".to_owned(), "xrocket".to_owned()]);
    transport.shutdown().await;

    // Боты сохранены в зашифрованной сессии: следующий старт обойдётся без resolve.
    let sealed = store.load().await.unwrap().unwrap();
    let session = EncryptedSession::open(&key(), ACCOUNT, &sealed, [CB, XR]).unwrap();
    assert_eq!(session.user(CB).and_then(|u| u.access_hash), Some(CB_HASH));
    assert_eq!(session.user(XR).and_then(|u| u.access_hash), Some(XR_HASH));
}

#[tokio::test]
async fn connect_aborts_when_username_points_to_another_id() {
    let fake = fake(|req| {
        let r = req.parse::<tl::functions::contacts::ResolveUsername>()?;
        let resolved: tl::enums::contacts::ResolvedPeer = tl::types::contacts::ResolvedPeer {
            peer: peer_user(42),
            chats: Vec::new(),
            users: vec![user(42, 1, true, Some(&r.username))],
        }
        .into();
        Some(Reply::ok(&resolved))
    })
    .await;
    let err = GrammersTransport::connect(config(), key(), store_for(&fake, false))
        .await
        .err()
        .unwrap();
    assert!(matches!(err, ConnectError::BotIdentity(_)), "{err:?}");
}

#[tokio::test]
async fn connect_aborts_when_pinned_id_has_another_username() {
    let fake = fake(|req| {
        let r = req.parse::<tl::functions::users::GetUsers>()?;
        let is_cb = matches!(r.id.first(), Some(tl::enums::InputUser::User(u)) if u.user_id == CB);
        is_cb.then(|| Reply::ok(&vec![user(CB, CB_HASH, true, Some("send_wallet_fake"))]))
    })
    .await;
    let err = GrammersTransport::connect(config(), key(), store_for(&fake, true))
        .await
        .err()
        .unwrap();
    assert!(matches!(err, ConnectError::BotIdentity(_)), "{err:?}");
}

#[tokio::test]
async fn revoked_session_is_not_authorized() {
    let fake = fake(|req| {
        req.is::<tl::functions::users::GetUsers>()
            .then(|| Reply::error(401, "AUTH_KEY_UNREGISTERED"))
    })
    .await;
    let err = GrammersTransport::connect(config(), key(), store_for(&fake, true))
        .await
        .err()
        .unwrap();
    assert!(matches!(err, ConnectError::NotAuthorized), "{err:?}");
}

#[tokio::test]
async fn send_text_uses_the_given_random_id_and_maps_duplicates() {
    let seen = Arc::new(Mutex::new(Vec::<i64>::new()));
    let s = Arc::clone(&seen);
    let fake = fake(move |req| {
        let m = req.parse::<tl::functions::messages::SendMessage>()?;
        let mut seen = s.lock().unwrap();
        let duplicate = seen.contains(&m.random_id);
        seen.push(m.random_id);
        Some(if duplicate {
            Reply::error(500, "RANDOM_ID_DUPLICATE")
        } else {
            Reply::ok(&short_sent(50))
        })
    })
    .await;
    let transport = connect(&fake).await;
    let chat = Chat::WalletBot(Platform::CryptoBot);

    let sent = transport
        .send_text(chat, "/start CQAbCdEfGhIj", -987_654_321)
        .await
        .unwrap();
    assert_eq!(sent.id, 50);
    assert!(sent.out);
    assert_eq!(sent.chat, chat);
    assert_eq!(sent.text, "/start CQAbCdEfGhIj");

    let again = transport
        .send_text(chat, "/start CQAbCdEfGhIj", -987_654_321)
        .await;
    assert_eq!(again, Err(TransportError::RandomIdDuplicate));

    let requests = fake.parsed::<tl::functions::messages::SendMessage>();
    assert_eq!(requests.len(), 2);
    for r in &requests {
        assert_eq!(r.random_id, -987_654_321);
        assert!(r.no_webpage);
        assert_eq!(
            r.peer,
            tl::types::InputPeerUser {
                user_id: CB,
                access_hash: CB_HASH
            }
            .into()
        );
    }
    transport.shutdown().await;
}

#[tokio::test]
async fn executed_but_unanswered_send_is_outcome_unknown_and_safe_to_repeat() {
    let seen = Arc::new(Mutex::new(Vec::<i64>::new()));
    let s = Arc::clone(&seen);
    let fake = fake(move |req| {
        let m = req.parse::<tl::functions::messages::SendMessage>()?;
        let mut seen = s.lock().unwrap();
        let duplicate = seen.contains(&m.random_id);
        seen.push(m.random_id);
        // Первый раз сервер «выполнил», но ответ потерялся.
        Some(if duplicate {
            Reply::error(500, "RANDOM_ID_DUPLICATE")
        } else {
            Reply::Silence
        })
    })
    .await;
    let transport = connect(&fake).await;
    let chat = Chat::WalletBot(Platform::XRocket);
    let connections_before = fake.connections();

    let first = transport
        .send_text(chat, "/start t_ABCDEFGHIJKLMNO", 77)
        .await;
    assert_eq!(first, Err(TransportError::Timeout));
    assert!(first.unwrap_err().outcome_unknown());
    // Транспорт сам не повторяет.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fake.count::<tl::functions::messages::SendMessage>(), 1);

    // Повтор вызывающим — тем же random_id — по новому соединению.
    let second = transport
        .send_text(chat, "/start t_ABCDEFGHIJKLMNO", 77)
        .await;
    assert_eq!(second, Err(TransportError::RandomIdDuplicate));
    assert_eq!(*seen.lock().unwrap(), vec![77, 77]);
    assert!(
        fake.connections() > connections_before,
        "the stalled connection was replaced"
    );
    transport.shutdown().await;
}

/// Ответ бота, отправленный, пока соединение «висело», не теряется: после таймаута пул
/// пересоздаётся, а новый поток апдейтов догоняет его через `updates.getDifference`.
#[tokio::test]
async fn reply_missed_during_a_stalled_request_is_caught_up() {
    let stalled = Arc::new(Mutex::new((false, false))); // (запрос завис, догон отдан)
    let s = Arc::clone(&stalled);
    let fake = fake(move |req| {
        let mut state = s.lock().unwrap();
        if req.is::<tl::functions::messages::SendMessage>() {
            state.0 = true;
            return Some(Reply::Silence);
        }
        if req.is::<tl::functions::updates::GetDifference>() && state.0 && !state.1 {
            state.1 = true;
            let difference: tl::enums::updates::Difference = tl::types::updates::Difference {
                new_messages: vec![message(60, CB, false, "Вы получили 3 USDT").into()],
                new_encrypted_messages: Vec::new(),
                other_updates: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
                state: tl::types::updates::State {
                    pts: START_PTS + 1,
                    qts: 0,
                    date: now(),
                    seq: 0,
                    unread_count: 0,
                }
                .into(),
            }
            .into();
            return Some(Reply::ok(&difference));
        }
        None
    })
    .await;
    let transport = connect(&fake).await;
    let mut rx = transport.subscribe();

    let result = transport
        .send_text(
            Chat::WalletBot(Platform::CryptoBot),
            "/start CQAbCdEfGhIj",
            9,
        )
        .await;
    assert_eq!(result, Err(TransportError::Timeout));

    let caught_up = next(&mut rx).await;
    assert_eq!(caught_up.chat, Chat::WalletBot(Platform::CryptoBot));
    assert_eq!(caught_up.id, 60);
    assert_eq!(caught_up.text, "Вы получили 3 USDT");
    let diff = &fake.parsed::<tl::functions::updates::GetDifference>()[0];
    assert_eq!(
        diff.pts, START_PTS,
        "catch-up starts from the persisted pts"
    );
    transport.shutdown().await;
}

#[tokio::test]
async fn flood_wait_is_returned_not_slept() {
    let fake = fake(|req| {
        req.is::<tl::functions::messages::SendMessage>()
            .then(|| Reply::error(420, "FLOOD_WAIT_42"))
    })
    .await;
    let transport = connect(&fake).await;
    let started = tokio::time::Instant::now();
    let result = transport
        .send_text(Chat::WalletBot(Platform::CryptoBot), "/start CQx", 1)
        .await;
    assert_eq!(
        result,
        Err(TransportError::FloodWait(Duration::from_secs(42)))
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(fake.count::<tl::functions::messages::SendMessage>(), 1);
    transport.shutdown().await;
}

#[tokio::test]
async fn button_press_is_never_repeated_after_a_broken_connection() {
    let fake = fake(|req| {
        req.is::<tl::functions::messages::GetBotCallbackAnswer>()
            .then_some(Reply::Close)
    })
    .await;
    let transport = connect(&fake).await;
    let result = transport
        .press(Chat::WalletBot(Platform::CryptoBot), 42, b"pay:usdt")
        .await;
    let err = result.unwrap_err();
    assert!(err.outcome_unknown(), "{err:?}");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let presses = fake.parsed::<tl::functions::messages::GetBotCallbackAnswer>();
    assert_eq!(
        presses.len(),
        1,
        "grammers must not retry the press after Io"
    );
    assert_eq!(presses[0].msg_id, 42);
    assert_eq!(presses[0].data.as_deref(), Some(b"pay:usdt".as_slice()));
    transport.shutdown().await;
}

#[tokio::test]
async fn bot_response_timeout_on_press_is_outcome_unknown() {
    let fake = fake(|req| {
        req.is::<tl::functions::messages::GetBotCallbackAnswer>()
            .then(|| Reply::error(400, "BOT_RESPONSE_TIMEOUT"))
    })
    .await;
    let transport = connect(&fake).await;
    let result = transport
        .press(Chat::WalletBot(Platform::XRocket), 7, b"x")
        .await;
    assert_eq!(result, Err(TransportError::Timeout));
    transport.shutdown().await;
}

#[tokio::test]
async fn inline_check_goes_to_saved_messages_with_the_given_random_id() {
    let fake = fake(|req| {
        if let Some(q) = req.parse::<tl::functions::messages::GetInlineBotResults>() {
            assert_eq!(q.peer, tl::enums::InputPeer::PeerSelf);
            let send_message: tl::enums::BotInlineMessage = tl::types::BotInlineMessageText {
                no_webpage: false,
                invert_media: false,
                message: "Чек на 10 USDT".into(),
                entities: None,
                reply_markup: None,
            }
            .into();
            let results: tl::enums::messages::BotResults = tl::types::messages::BotResults {
                gallery: false,
                query_id: 555,
                next_offset: None,
                switch_pm: None,
                switch_webview: None,
                results: vec![
                    tl::types::BotInlineResult {
                        id: "usdt-10".into(),
                        r#type: "article".into(),
                        title: Some("Отправить 10 USDT".into()),
                        description: None,
                        url: None,
                        thumb: None,
                        content: None,
                        send_message,
                    }
                    .into(),
                ],
                cache_time: 0,
                users: Vec::new(),
            }
            .into();
            return Some(Reply::ok(&results));
        }
        let s = req.parse::<tl::functions::messages::SendInlineBotResult>()?;
        let mut msg = message(90, SELF_ID, true, "🦋 Чек на 10 USDT");
        msg.via_bot_id = Some(CB);
        msg.reply_markup = Some(inline_markup(vec![vec![url_button(
            "Получить 10 USDT",
            "https://t.me/send?start=CQAbCdEfGhIj",
        )]]));
        Some(Reply::ok(&updates(vec![
            tl::types::UpdateMessageId {
                id: 90,
                random_id: s.random_id,
            }
            .into(),
            new_message(msg, START_PTS + 60),
        ])))
    })
    .await;
    let transport = connect(&fake).await;

    let results = transport
        .inline_query(Platform::CryptoBot, "10usdt")
        .await
        .unwrap();
    assert_eq!(results.query_id, 555);
    assert_eq!(results.results[0].id, "usdt-10");

    let unknown = transport
        .send_inline(
            Chat::SavedMessages,
            Platform::CryptoBot,
            &results,
            "nope",
            1,
        )
        .await;
    assert!(matches!(unknown, Err(TransportError::Other(_))));
    assert_eq!(
        fake.count::<tl::functions::messages::SendInlineBotResult>(),
        0
    );

    let sent = transport
        .send_inline(
            Chat::SavedMessages,
            Platform::CryptoBot,
            &results,
            "usdt-10",
            4242,
        )
        .await
        .unwrap();
    assert_eq!(sent.chat, Chat::SavedMessages);
    assert_eq!(sent.id, 90);
    assert_eq!(sent.via_bot, Some(Platform::CryptoBot));
    assert_eq!(
        sent.button_urls(),
        vec!["https://t.me/send?start=CQAbCdEfGhIj".to_owned()]
    );

    let requests = fake.parsed::<tl::functions::messages::SendInlineBotResult>();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].random_id, 4242);
    assert_eq!(requests[0].query_id, 555);
    assert_eq!(requests[0].id, "usdt-10");
    assert_eq!(requests[0].peer, tl::enums::InputPeer::PeerSelf);
    let queries = fake.parsed::<tl::functions::messages::GetInlineBotResults>();
    assert_eq!(
        queries[0].bot,
        tl::types::InputUser {
            user_id: CB,
            access_hash: CB_HASH
        }
        .into()
    );
    transport.shutdown().await;
}

#[tokio::test]
async fn history_and_webapp_buttons() {
    let fake = fake(|req| {
        if req.is::<tl::functions::messages::GetHistory>() {
            let mut card = message(31, CB, false, "Счёт на 5 USDT");
            card.reply_markup = Some(inline_markup(vec![vec![
                tl::types::KeyboardButtonSimpleWebView {
                    style: None,
                    text: "Оплатить".into(),
                    url: "https://app.send.tg/pay/simple".into(),
                }
                .into(),
            ]]));
            let messages: tl::enums::messages::Messages = tl::types::messages::Messages {
                messages: vec![
                    card.into(),
                    message(30, CB, true, "/start IVabc").into(),
                    message(29, CB, false, "старое").into(),
                ],
                topics: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
            }
            .into();
            return Some(Reply::ok(&messages));
        }
        let web: tl::enums::WebViewResult = tl::types::WebViewResultUrl {
            fullsize: false,
            fullscreen: false,
            same_origin: false,
            query_id: None,
            url: "https://app.send.tg/#tgWebAppData=signed".into(),
        }
        .into();
        (req.is::<tl::functions::messages::RequestSimpleWebView>()
            || req.is::<tl::functions::messages::RequestWebView>())
        .then(|| Reply::ok(&web))
    })
    .await;
    let transport = connect(&fake).await;
    let chat = Chat::WalletBot(Platform::CryptoBot);

    let history = transport.history(chat, 29, 20).await.unwrap();
    assert_eq!(
        history.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![30, 31]
    );
    assert_eq!(
        history[1].buttons[0][0].kind,
        ButtonKind::WebApp("https://app.send.tg/pay/simple".into())
    );
    let req = &fake.parsed::<tl::functions::messages::GetHistory>()[0];
    assert_eq!(
        (req.offset_id, req.add_offset, req.limit, req.min_id),
        (30, -20, 20, 29)
    );

    // Кнопка была SimpleWebView — значит requestSimpleWebView.
    let url = transport
        .open_webapp(Platform::CryptoBot, "https://app.send.tg/pay/simple")
        .await
        .unwrap();
    assert_eq!(url, "https://app.send.tg/#tgWebAppData=signed");
    assert_eq!(
        fake.count::<tl::functions::messages::RequestSimpleWebView>(),
        1
    );
    // Неизвестная кнопка — requestWebView с peer бота.
    transport
        .open_webapp(Platform::CryptoBot, "https://app.send.tg/other")
        .await
        .unwrap();
    let web = &fake.parsed::<tl::functions::messages::RequestWebView>()[0];
    assert_eq!(web.url.as_deref(), Some("https://app.send.tg/other"));
    assert_eq!(
        web.peer,
        tl::types::InputPeerUser {
            user_id: CB,
            access_hash: CB_HASH
        }
        .into()
    );
    // Мини-приложение чужого бота не открываем.
    let foreign = transport
        .open_webapp(Platform::CryptoBot, "https://t.me/evil_bot/app?startapp=x")
        .await;
    assert!(matches!(foreign, Err(TransportError::Other(_))));
    transport.shutdown().await;
}

#[tokio::test]
async fn update_stream_delivers_only_wallet_chats() {
    let fake = fake(|_| None).await;
    let transport = connect(&fake).await;
    let mut rx = transport.subscribe();

    // Чужой человек пишет аккаунту — не наш чат.
    fake.push(&updates(vec![new_message(
        message(9, 123_456, false, "привет"),
        START_PTS + 1,
    )]));
    // Ответ CryptoBot с кнопкой.
    let mut reply = message(10, CB, false, "Вы получили 10 USDT");
    reply.reply_markup = Some(inline_markup(vec![vec![url_button(
        "Кошелёк",
        "https://t.me/send?start=wallet",
    )]]));
    fake.push(&updates(vec![new_message(reply.clone(), START_PTS + 2)]));
    let got = next(&mut rx).await;
    assert_eq!(got.chat, Chat::WalletBot(Platform::CryptoBot));
    assert_eq!(got.id, 10);
    assert_eq!(got.text, "Вы получили 10 USDT");
    assert_eq!(
        got.button_urls(),
        vec!["https://t.me/send?start=wallet".to_owned()]
    );

    // Короткая форма от xRocket (без кнопок).
    let short: tl::enums::Updates = tl::types::UpdateShortMessage {
        out: false,
        mentioned: false,
        media_unread: false,
        silent: false,
        id: 11,
        user_id: XR,
        message: "Чек не найден".into(),
        pts: START_PTS + 3,
        pts_count: 1,
        date: now(),
        fwd_from: None,
        via_bot_id: None,
        reply_to: None,
        entities: None,
        ttl_period: None,
    }
    .into();
    fake.push(&short);
    let got = next(&mut rx).await;
    assert_eq!(got.chat, Chat::WalletBot(Platform::XRocket));
    assert_eq!(got.text, "Чек не найден");
    assert!(got.buttons.is_empty());

    // Правка ответа CryptoBot.
    reply.message = "Вы получили 10 USDT (обновлено)".into();
    reply.edit_date = Some(now());
    fake.push(&updates(vec![edit_message(reply, START_PTS + 4)]));
    let got = next(&mut rx).await;
    assert_eq!(got.id, 10);
    assert!(got.edit_date.is_some());

    // Инлайн-чек в «Избранном».
    let mut saved = message(12, SELF_ID, true, "🚀 Чек на 10 USDT");
    saved.via_bot_id = Some(XR);
    fake.push(&updates(vec![new_message(saved, START_PTS + 5)]));
    let got = next(&mut rx).await;
    assert_eq!(got.chat, Chat::SavedMessages);
    assert_eq!(got.via_bot, Some(Platform::XRocket));

    assert!(rx.try_recv().is_err(), "nothing else was delivered");
    transport.shutdown().await;
}

#[tokio::test]
async fn reconnects_after_the_server_drops_connections() {
    let fake = fake(|_| None).await;
    let transport = connect(&fake).await;
    let mut rx = transport.subscribe();
    let before = fake.connections();

    fake.kick();
    // Сторож пингует и открывает новое соединение.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while fake.connections() == before {
        assert!(tokio::time::Instant::now() < deadline, "no reconnect");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // После разрыва клиент догоняет апдейты (getDifference), затем принимает новые.
    fake.wait_for("updates.getDifference", |r| {
        r.iter()
            .any(Request::is::<tl::functions::updates::GetDifference>)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    fake.push(&updates(vec![new_message(
        message(20, CB, false, "после переподключения"),
        START_PTS + 1,
    )]));
    let got = next(&mut rx).await;
    assert_eq!(got.text, "после переподключения");

    let sent = transport.send_text(Chat::SavedMessages, "заметка", 5).await;
    // Ответ на sendMessage сервер не настроил — явный отказ, не «неизвестно».
    assert!(
        matches!(sent, Err(TransportError::Rpc { code: 400, .. })),
        "{sent:?}"
    );
    assert!(transport.is_connected());
    transport.shutdown().await;
}

#[tokio::test]
async fn auth_loss_after_start_fails_fast() {
    let fake = fake(|req| {
        req.is::<tl::functions::messages::SendMessage>()
            .then(|| Reply::error(401, "SESSION_REVOKED"))
    })
    .await;
    let transport = connect(&fake).await;
    let chat = Chat::WalletBot(Platform::CryptoBot);
    assert_eq!(
        transport.send_text(chat, "/start CQx", 1).await,
        Err(TransportError::NotAuthorized)
    );
    assert!(!transport.is_authorized());
    // Следующий вызов не уходит в сеть.
    assert_eq!(
        transport.send_text(chat, "/start CQy", 2).await,
        Err(TransportError::NotAuthorized)
    );
    assert_eq!(fake.count::<tl::functions::messages::SendMessage>(), 1);
    transport.shutdown().await;
}

#[tokio::test]
async fn session_is_saved_encrypted_with_update_state() {
    let fake = fake(|_| None).await;
    let store = store_for(&fake, true);
    let transport = GrammersTransport::connect(config(), key(), Arc::clone(&store) as _)
        .await
        .unwrap();
    fake.wait_for("updates.getState", |r| {
        r.iter()
            .any(Request::is::<tl::functions::updates::GetState>)
    })
    .await;
    let mut rx = transport.subscribe();
    fake.push(&updates(vec![new_message(
        message(15, CB, false, "x"),
        START_PTS + 1,
    )]));
    let _ = next(&mut rx).await;
    transport.shutdown().await;

    let sealed = store.load().await.unwrap().unwrap();
    assert!(
        !sealed
            .ciphertext
            .windows(32)
            .any(|w| w == &fake.auth_key()[..32]),
        "auth key is never stored in clear"
    );
    let session = EncryptedSession::open(&key(), ACCOUNT, &sealed, [CB, XR]).unwrap();
    let snapshot = session.snapshot().unwrap();
    assert_eq!(snapshot.self_user().map(|u| u.id), Some(SELF_ID));
    assert!(
        snapshot.updates.pts > START_PTS,
        "pts is persisted for catch-up after restart: {}",
        snapshot.updates.pts
    );
}
