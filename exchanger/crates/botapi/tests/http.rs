//! Клиент против локального HTTP-сервера на tokio: путь и тело запроса, разбор `ok`,
//! ошибок Telegram, `retry_after`, не-JSON ответа, таймаута и отказа в соединении.
//! Главная проверка — токен не попадает ни в `Display`, ни в `Debug` ошибки.
#![cfg(feature = "client")]
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use botapi::{
    BotApi, Error, GetUpdates, InlineKeyboardButton, InlineKeyboardMarkup, SendMessage,
    TransportKind,
};
use secrecy::SecretString;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const TOKEN: &str = "123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw";

#[derive(Debug)]
struct Recorded {
    request_line: String,
    content_type: Option<String>,
    body: serde_json::Value,
}

enum Reply {
    Respond {
        status: u16,
        body: String,
    },
    /// Принять запрос и молчать — для проверки таймаута.
    Hang,
}

fn ok(body: &str) -> Reply {
    Reply::Respond {
        status: 200,
        body: body.to_owned(),
    }
}

fn status(status: u16, body: &str) -> Reply {
    Reply::Respond {
        status,
        body: body.to_owned(),
    }
}

/// Сервер отвечает на соединения по очереди заранее заданными ответами.
async fn serve(replies: Vec<Reply>) -> (String, JoinHandle<Vec<Recorded>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let mut recorded = Vec::new();
        for reply in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            recorded.push(read_request(&mut socket).await);
            match reply {
                Reply::Respond { status, body } => {
                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                    socket.shutdown().await.ok();
                }
                Reply::Hang => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
        recorded
    });
    (base, handle)
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Recorded {
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let n = socket.read(&mut chunk).await.unwrap();
        assert!(n > 0, "клиент закрыл соединение до конца заголовков");
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8(buf[..header_end].to_vec()).unwrap();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap().to_owned();
    let mut content_length = 0;
    let mut content_type = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            match name.trim().to_ascii_lowercase().as_str() {
                "content-length" => content_length = value.trim().parse().unwrap(),
                "content-type" => content_type = Some(value.trim().to_owned()),
                _ => {}
            }
        }
    }
    while buf.len() < header_end + content_length {
        let n = socket.read(&mut chunk).await.unwrap();
        assert!(n > 0, "клиент закрыл соединение до конца тела");
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = serde_json::from_slice(&buf[header_end..header_end + content_length]).unwrap();
    Recorded {
        request_line,
        content_type,
        body,
    }
}

fn client(base: &str) -> BotApi {
    BotApi::builder(SecretString::from(TOKEN))
        .base_url(base)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

fn assert_no_token(err: &Error) {
    let text = format!("{err} | {err:?}");
    assert!(!text.contains(TOKEN), "токен в ошибке: {text}");
    assert!(!text.contains("AAHdqTcv"), "часть токена в ошибке: {text}");
}

#[tokio::test]
async fn get_me_posts_json_to_token_path() {
    let (base, server) = serve(vec![ok(
        r#"{"ok":true,"result":{"id":42,"is_bot":true,"first_name":"Exch","username":"exch_bot"}}"#,
    )])
    .await;
    let me = client(&base).get_me().await.unwrap();
    assert_eq!(me.username.as_deref(), Some("exch_bot"));

    let recorded = server.await.unwrap();
    assert_eq!(
        recorded[0].request_line,
        format!("POST /bot{TOKEN}/getMe HTTP/1.1")
    );
    assert_eq!(
        recorded[0].content_type.as_deref(),
        Some("application/json")
    );
    assert_eq!(recorded[0].body, serde_json::json!({}));
}

#[tokio::test]
async fn send_message_body_reaches_server_and_message_is_parsed() {
    let (base, server) = serve(vec![ok(
        r#"{"ok":true,"result":{"message_id":501,"chat":{"id":777,"type":"private"},"date":1790000000,"text":"Готово!"}}"#,
    )])
    .await;
    let request =
        SendMessage::html(777, "<b>Готово!</b>").reply_markup(InlineKeyboardMarkup::new(vec![
            vec![InlineKeyboardButton::copy_text(
                "📋 Номер заявки",
                "A1B2C3D4E5",
            )],
        ]));
    let message = client(&base).send_message(&request).await.unwrap();
    assert_eq!(message.message_id, 501);

    let recorded = server.await.unwrap();
    assert_eq!(
        recorded[0].request_line,
        format!("POST /bot{TOKEN}/sendMessage HTTP/1.1")
    );
    assert_eq!(recorded[0].body, serde_json::to_value(&request).unwrap());
}

#[tokio::test]
async fn telegram_error_with_retry_after() {
    let (base, server) = serve(vec![status(
        429,
        r#"{"ok":false,"error_code":429,"description":"Too Many Requests: retry after 12","parameters":{"retry_after":12}}"#,
    )])
    .await;
    let err = client(&base)
        .send_message(&SendMessage::html(1, "x"))
        .await
        .unwrap_err();
    server.await.unwrap();
    assert!(err.is_flood());
    assert_eq!(err.retry_after(), Some(Duration::from_secs(12)));
    assert!(!err.outcome_unknown() && !err.not_delivered());
    assert_no_token(&err);
}

#[tokio::test]
async fn telegram_rejection_and_not_modified() {
    let (base, server) = serve(vec![
        status(
            400,
            r#"{"ok":false,"error_code":400,"description":"Bad Request: can't parse entities: Unsupported start tag \"h3\" at byte offset 0"}"#,
        ),
        status(
            400,
            r#"{"ok":false,"error_code":400,"description":"Bad Request: message is not modified"}"#,
        ),
    ])
    .await;
    let api = client(&base);
    let rejected = api
        .send_message(&SendMessage::html(1, "<h3>x</h3>"))
        .await
        .unwrap_err();
    assert!(rejected.is_rejection());
    assert!(
        rejected
            .description()
            .unwrap()
            .contains("Unsupported start tag")
    );

    let not_modified = api
        .edit_message_rich(1, 2, "<p>same</p>", None)
        .await
        .unwrap_err();
    assert!(not_modified.is_not_modified());

    let recorded = server.await.unwrap();
    assert_eq!(
        recorded[1].body,
        serde_json::json!({
            "chat_id": 1, "message_id": 2,
            "rich_message": {"html": "<p>same</p>", "skip_entity_detection": true}
        })
    );
    assert_no_token(&rejected);
}

#[tokio::test]
async fn proxy_page_echoing_the_url_is_scrubbed() {
    let page = format!("<html>502 Bad Gateway for /bot{TOKEN}/getMe</html>");
    let (base, server) = serve(vec![status(502, &page)]).await;
    let err = client(&base).get_me().await.unwrap_err();
    server.await.unwrap();
    assert!(matches!(err, Error::Decode { status: 502, .. }), "{err:?}");
    assert!(err.outcome_unknown());
    assert!(err.to_string().contains("<redacted>"));
    assert_no_token(&err);
}

#[tokio::test]
async fn ok_with_unexpected_result_is_a_decode_error() {
    let (base, server) = serve(vec![ok(r#"{"ok":true,"result":true}"#)]).await;
    let err = client(&base).get_me().await.unwrap_err();
    server.await.unwrap();
    assert!(matches!(err, Error::Decode { status: 200, .. }), "{err:?}");
}

#[tokio::test]
async fn long_poll_skips_broken_update_and_sends_offset() {
    let (base, server) = serve(vec![ok(
        r#"{"ok":true,"result":[
            {"update_id":10,"message":{"message_id":1,"chat":{"id":5,"type":"private"},"date":1,"text":"/start"}},
            {"update_id":11,"message":{"message_id":"broken"}},
            {"update_id":12,"poll":{"id":"x"}}
        ]}"#,
    )])
    .await;
    let updates = client(&base)
        .get_updates(&GetUpdates::after(Some(9), 1))
        .await
        .unwrap();
    let ids: Vec<i64> = updates.iter().map(|u| u.update_id).collect();
    assert_eq!(ids, vec![10, 11, 12]);
    assert_eq!(
        updates[0].message.as_ref().and_then(|m| m.text.as_deref()),
        Some("/start")
    );

    let recorded = server.await.unwrap();
    assert_eq!(
        recorded[0].body,
        serde_json::json!({"offset": 10, "timeout": 1})
    );
}

#[tokio::test]
async fn timeout_after_sending_means_outcome_unknown() {
    let (base, _server) = serve(vec![Reply::Hang]).await;
    let api = BotApi::builder(SecretString::from(TOKEN))
        .base_url(&base)
        .timeout(Duration::from_millis(300))
        .build()
        .unwrap();
    let err = api
        .send_message(&SendMessage::html(1, "x"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::Transport {
                kind: TransportKind::Timeout,
                ..
            }
        ),
        "{err:?}"
    );
    assert!(err.outcome_unknown());
    assert!(!err.not_delivered());
    assert_no_token(&err);
}

#[tokio::test]
async fn refused_connection_means_not_delivered() {
    // Порт, на котором точно никто не слушает: заняли и сразу освободили.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let err = client(&format!("http://{addr}"))
        .get_me()
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::Transport {
                kind: TransportKind::Connect,
                ..
            }
        ),
        "{err:?}"
    );
    assert!(err.not_delivered());
    assert!(!err.outcome_unknown());
    assert_no_token(&err);
}
