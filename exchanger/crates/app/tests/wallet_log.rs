//! Журнал сообщений ботов кошельков (`app::wallet_log`) на живом PostgreSQL: чат → id бота
//! или свой id, ключ операции → `operation_id`, ошибки БД не роняют и не держат сценарий.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use app::wallet_log::{self, AccountIds, WalletLog, WalletLogError};
use domain::Platform;
use mtproto::PinnedBots;
use serde_json::json;
use sqlx::{PgPool, Row};
use userbot::transport::{ButtonKind, RawButton};
use userbot::{Chat, MessageLog, RawMessage};

const LABEL: &str = "ub-1";
const SELF_ID: i64 = 7001;

fn msg(chat: Chat, id: i32) -> RawMessage {
    RawMessage {
        chat,
        id,
        out: false,
        date: 1_760_000_000,
        edit_date: None,
        text: "Вы получили 1 USDT".into(),
        entity_urls: vec![],
        buttons: vec![],
        via_bot: None,
    }
}

async fn account(pool: &PgPool, tg_user_id: Option<i64>) -> i16 {
    let mut conn = pool.acquire().await.unwrap();
    let id = storage::wallets::ensure_userbot(&mut conn, LABEL, "+31*******42")
        .await
        .unwrap();
    if let Some(tg) = tg_user_id {
        storage::sessions::set_tg_user(&mut conn, LABEL, tg)
            .await
            .unwrap();
    }
    id
}

async fn rows(pool: &PgPool) -> Vec<sqlx::postgres::PgRow> {
    sqlx::query(
        "SELECT userbot_id, bot_peer_id, message_id, edit_date, is_outgoing, text, entities, \
                reply_markup, operation_id FROM wallet_messages ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[test]
fn chats_map_to_pinned_bots_and_own_id() {
    let bots = PinnedBots::default();
    assert_eq!(
        wallet_log::peer_id(Chat::WalletBot(Platform::CryptoBot), &bots, Some(SELF_ID)),
        Some(1_559_501_630)
    );
    assert_eq!(
        wallet_log::peer_id(Chat::WalletBot(Platform::XRocket), &bots, None),
        Some(5_014_831_088)
    );
    assert_eq!(
        wallet_log::peer_id(Chat::SavedMessages, &bots, Some(SELF_ID)),
        Some(SELF_ID)
    );
    assert_eq!(wallet_log::peer_id(Chat::SavedMessages, &bots, None), None);
}

#[test]
fn row_carries_urls_buttons_via_bot_and_edit_date() {
    let mut m = msg(Chat::SavedMessages, 5);
    m.out = true;
    m.edit_date = Some(1_760_000_100);
    m.entity_urls = vec!["https://t.me/send?start=CQabc".into()];
    m.via_bot = Some(Platform::CryptoBot);
    m.buttons = vec![vec![RawButton {
        text: "Получить".into(),
        kind: ButtonKind::Url("https://t.me/send?start=CQabc".into()),
    }]];
    let row = wallet_log::to_row(3, SELF_ID, &m, Some("ord-1-payout")).unwrap();
    assert_eq!(row.userbot_id, 3);
    assert_eq!(row.bot_peer_id, SELF_ID);
    assert_eq!(row.message_id, 5);
    assert_eq!(row.edit_date, 1_760_000_100);
    assert!(row.is_outgoing);
    assert_eq!(row.operation_key, Some("ord-1-payout"));
    assert_eq!(
        row.entities,
        Some(json!({
            "urls": ["https://t.me/send?start=CQabc"],
            "date": 1_760_000_000,
            "via_bot": "cryptobot",
        }))
    );
    assert_eq!(
        row.reply_markup,
        Some(json!([[{ "text": "Получить", "kind": { "Url": "https://t.me/send?start=CQabc" } }]]))
    );

    let original = msg(Chat::SavedMessages, 6);
    let plain = wallet_log::to_row(3, 1, &original, None).unwrap();
    assert_eq!(plain.edit_date, 0, "original version");
    assert_eq!(plain.reply_markup, None, "no buttons, no markup");
    assert_eq!(
        plain.entities,
        Some(json!({ "urls": [], "date": 1_760_000_000 }))
    );

    let mut far = msg(Chat::SavedMessages, 7);
    far.edit_date = Some(i64::from(i32::MAX) + 1);
    assert!(matches!(
        wallet_log::to_row(3, 1, &far, None),
        Err(WalletLogError::OutOfRange("edit_date"))
    ));
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn messages_are_journaled_per_chat(pool: PgPool) {
    let ub = account(&pool, Some(SELF_ID)).await;
    let log = WalletLog::new(pool.clone(), PinnedBots::default());

    // Аккаунт не зарегистрирован — журнал находит его в БД по метке.
    log.record(LABEL, &msg(Chat::WalletBot(Platform::CryptoBot), 10), None)
        .await;
    log.record(LABEL, &msg(Chat::WalletBot(Platform::XRocket), 10), None)
        .await;
    log.record(LABEL, &msg(Chat::SavedMessages, 10), None).await;
    // Правка — новая строка; повтор той же версии — нет.
    let mut edited = msg(Chat::WalletBot(Platform::CryptoBot), 10);
    edited.edit_date = Some(1_760_000_050);
    edited.text = "Вы получили 2 USDT".into();
    log.record(LABEL, &edited, None).await;
    log.record(LABEL, &edited, None).await;

    let rows = rows(&pool).await;
    let got: Vec<(i16, i64, i32, i32, String)> = rows
        .iter()
        .map(|r| {
            (
                r.get("userbot_id"),
                r.get("bot_peer_id"),
                r.get("message_id"),
                r.get("edit_date"),
                r.get("text"),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            (ub, 1_559_501_630, 10, 0, "Вы получили 1 USDT".into()),
            (ub, 5_014_831_088, 10, 0, "Вы получили 1 USDT".into()),
            (ub, SELF_ID, 10, 0, "Вы получили 1 USDT".into()),
            (
                ub,
                1_559_501_630,
                10,
                1_760_000_050,
                "Вы получили 2 USDT".into()
            ),
        ]
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn operation_key_links_the_operation(pool: PgPool) {
    let ub = account(&pool, None).await;
    let op: i64 = sqlx::query_scalar(
        "INSERT INTO operations (kind, money_role, platform, idempotency_key) \
         VALUES ('read_balance', 'aux', 'cryptobot', 'bal-cb-000001') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let log = WalletLog::new(pool.clone(), PinnedBots::default());
    log.register(
        LABEL,
        AccountIds {
            userbot_id: ub,
            self_id: Some(SELF_ID),
        },
    );

    let m = msg(Chat::WalletBot(Platform::CryptoBot), 20);
    // До разбора — без операции, после — с ключом: та же строка получает operation_id.
    let first = log.try_record(LABEL, &m, None).await.unwrap();
    assert_eq!(first.operation_id, None);
    log.record(LABEL, &m, Some("bal-cb-000001")).await;
    // Ключ несуществующей операции не ломает запись.
    log.record(LABEL, &msg(Chat::SavedMessages, 21), Some("bal-cb-missing"))
        .await;

    let rows = rows(&pool).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get::<Option<i64>, _>("operation_id"), Some(op));
    assert_eq!(rows[1].get::<Option<i64>, _>("operation_id"), None);
    assert_eq!(
        rows[1].get::<i64, _>("bot_peer_id"),
        SELF_ID,
        "registered own id"
    );
}

/// Ошибки журнала — только предупреждения: неизвестная метка, нет своего id для «Избранного»,
/// закрытый пул. `record` возвращается, ничего не паникует.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn failures_never_reach_the_flow(pool: PgPool) {
    let log = WalletLog::new(pool.clone(), PinnedBots::default());
    assert!(matches!(
        log.try_record(
            "ub-nobody",
            &msg(Chat::WalletBot(Platform::CryptoBot), 1),
            None
        )
        .await,
        Err(WalletLogError::UnknownAccount(_))
    ));
    log.record(
        "ub-nobody",
        &msg(Chat::WalletBot(Platform::CryptoBot), 1),
        None,
    )
    .await;

    account(&pool, None).await;
    assert!(matches!(
        log.try_record(LABEL, &msg(Chat::SavedMessages, 2), None)
            .await,
        Err(WalletLogError::NoSelfId(_))
    ));
    log.record(LABEL, &msg(Chat::SavedMessages, 2), None).await;
    assert!(rows(&pool).await.is_empty());

    pool.close().await;
    let closed = WalletLog::new(pool.clone(), PinnedBots::default());
    assert!(matches!(
        closed
            .try_record(LABEL, &msg(Chat::WalletBot(Platform::XRocket), 3), None)
            .await,
        Err(WalletLogError::Pool(_))
    ));
    closed
        .record(LABEL, &msg(Chat::WalletBot(Platform::XRocket), 3), None)
        .await;
}

/// Зависшая БД держит сценарий не дольше таймаута журнала.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn slow_database_does_not_hold_the_flow(pool: PgPool) {
    let ub = account(&pool, Some(SELF_ID)).await;
    // Занять строку аккаунта: вставка в wallet_messages ждёт блокировку FK (KEY SHARE).
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM userbot_accounts WHERE id = $1 FOR UPDATE")
        .bind(ub)
        .fetch_one(&mut *blocker)
        .await
        .unwrap();

    let log = Arc::new(
        WalletLog::new(pool.clone(), PinnedBots::default())
            .with_timeout(Duration::from_millis(200)),
    );
    log.register(
        LABEL,
        AccountIds {
            userbot_id: ub,
            self_id: Some(SELF_ID),
        },
    );
    let started = std::time::Instant::now();
    log.record(LABEL, &msg(Chat::WalletBot(Platform::CryptoBot), 30), None)
        .await;
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(200),
        "the insert was not blocked ({waited:?}): the test proves nothing"
    );
    assert!(
        waited < Duration::from_secs(2),
        "record returned after {waited:?}"
    );
    blocker.rollback().await.unwrap();
}
