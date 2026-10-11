//! Аккаунты юзерботов (сессия, статус, user id), привязка кошельков и журнал `wallet_messages`
//! на живом PostgreSQL.

#![allow(clippy::unwrap_used)]

use domain::Platform;
use serde_json::json;
use sqlx::{PgPool, Row};
use storage::StorageError;
use storage::sessions::{self, StoredSession, UserbotStatus};
use storage::wallet_messages::{self, NewWalletMessage};
use storage::wallets;

fn sample_session(fill: u8) -> StoredSession {
    StoredSession {
        ciphertext: vec![fill; 80],
        nonce: vec![fill ^ 0xFF; 24],
        key_version: 1,
    }
}

async fn userbot(pool: &PgPool, label: &str) -> i16 {
    let mut conn = pool.acquire().await.unwrap();
    wallets::ensure_userbot(&mut conn, label, "+31******01")
        .await
        .unwrap()
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn session_is_saved_and_loaded_as_bytes(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        sessions::load_session(&mut conn, "ub-cb-1").await.unwrap(),
        None,
        "unknown label"
    );
    let id = userbot(&pool, "ub-cb-1").await;
    assert_eq!(
        sessions::load_session(&mut conn, "ub-cb-1").await.unwrap(),
        None,
        "no login yet"
    );
    let account = sessions::find(&mut conn, "ub-cb-1").await.unwrap().unwrap();
    assert_eq!(account.id, id);
    assert_eq!(account.status, UserbotStatus::New);
    assert!(!account.has_session);
    assert_eq!(account.tg_user_id, None);

    let first = sample_session(0x11);
    sessions::save_session(&mut conn, "ub-cb-1", &first)
        .await
        .unwrap();
    assert_eq!(
        sessions::load_session(&mut conn, "ub-cb-1").await.unwrap(),
        Some(first)
    );
    // Новое сохранение заменяет старое целиком (шифротекст, nonce и версия ключа).
    let second = StoredSession {
        key_version: 2,
        ..sample_session(0x22)
    };
    sessions::save_session(&mut conn, "ub-cb-1", &second)
        .await
        .unwrap();
    assert_eq!(
        sessions::load_session(&mut conn, "ub-cb-1").await.unwrap(),
        Some(second)
    );
    assert!(
        sessions::find(&mut conn, "ub-cb-1")
            .await
            .unwrap()
            .unwrap()
            .has_session
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn writes_to_unknown_account_are_rejected(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    assert!(matches!(
        sessions::save_session(&mut conn, "ub-nobody", &sample_session(1)).await,
        Err(StorageError::UnknownUserbot(label)) if label == "ub-nobody"
    ));
    assert!(matches!(
        sessions::set_status(&mut conn, "ub-nobody", UserbotStatus::Active).await,
        Err(StorageError::UnknownUserbot(_))
    ));
    assert!(matches!(
        sessions::set_tg_user(&mut conn, "ub-nobody", 777).await,
        Err(StorageError::UnknownUserbot(_))
    ));
    assert_eq!(sessions::find(&mut conn, "ub-nobody").await.unwrap(), None);
}

/// Все статусы перечисления проходят CHECK схемы; `active` отмечает `last_ok_at`.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn every_status_matches_schema_check(pool: PgPool) {
    userbot(&pool, "ub-cb-1").await;
    let mut conn = pool.acquire().await.unwrap();
    for status in UserbotStatus::ALL {
        sessions::set_status(&mut conn, "ub-cb-1", status)
            .await
            .unwrap();
        let account = sessions::find(&mut conn, "ub-cb-1").await.unwrap().unwrap();
        assert_eq!(account.status, status);
        assert_eq!(UserbotStatus::from_db_name(status.db_name()), Some(status));
    }
    let last_ok: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT last_ok_at FROM userbot_accounts WHERE label = 'ub-cb-1'")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert!(last_ok.is_some(), "active sets last_ok_at");
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn one_telegram_account_per_label(pool: PgPool) {
    userbot(&pool, "ub-cb-1").await;
    userbot(&pool, "ub-xr-1").await;
    let mut conn = pool.acquire().await.unwrap();
    sessions::set_tg_user(&mut conn, "ub-cb-1", 7001)
        .await
        .unwrap();
    sessions::set_tg_user(&mut conn, "ub-cb-1", 7001)
        .await
        .unwrap();
    assert!(matches!(
        sessions::set_tg_user(&mut conn, "ub-xr-1", 7001).await,
        Err(StorageError::TelegramAccountInUse(7001))
    ));
    let account = sessions::find(&mut conn, "ub-cb-1").await.unwrap().unwrap();
    assert_eq!(account.tg_user_id, Some(7001));
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn wallet_binding_names_the_account(pool: PgPool) {
    let ub = userbot(&pool, "ub-cb-1").await;
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        wallets::wallet_binding(&mut conn, Platform::CryptoBot)
            .await
            .unwrap(),
        None
    );
    let wallet = wallets::ensure_wallet(&mut conn, Platform::CryptoBot, ub)
        .await
        .unwrap();
    let binding = wallets::wallet_binding(&mut conn, Platform::CryptoBot)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(binding.wallet_id, wallet);
    assert_eq!(binding.userbot_id, ub);
    assert_eq!(binding.userbot_label, "ub-cb-1");
    assert_eq!(
        wallets::wallet_binding(&mut conn, Platform::XRocket)
            .await
            .unwrap(),
        None
    );
}

#[test]
fn stored_session_debug_shows_no_bytes() {
    let dbg = format!("{:?}", sample_session(0xAB));
    assert!(dbg.contains("ciphertext_len: 80"), "{dbg}");
    assert!(!dbg.contains("171"), "{dbg}"); // 0xAB
    assert!(!dbg.contains("nonce"), "{dbg}");
}

fn message<'a>(userbot_id: i16, message_id: i32, edit_date: i32) -> NewWalletMessage<'a> {
    NewWalletMessage {
        userbot_id,
        bot_peer_id: 1_559_501_630,
        message_id,
        edit_date,
        is_outgoing: false,
        text: "Вы получили 1 USDT",
        entities: Some(json!({ "urls": [], "date": 1_760_000_000 })),
        reply_markup: None,
        operation_key: None,
    }
}

async fn read_balance_op(pool: &PgPool, key: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO operations (kind, money_role, platform, idempotency_key) \
         VALUES ('read_balance', 'aux', 'cryptobot', $1) RETURNING id",
    )
    .bind(key)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn message_versions_are_unique_and_idempotent(pool: PgPool) {
    let ub = userbot(&pool, "ub-cb-1").await;
    let mut conn = pool.acquire().await.unwrap();

    let first = wallet_messages::insert(&mut conn, &message(ub, 10, 0))
        .await
        .unwrap();
    let again = wallet_messages::insert(&mut conn, &message(ub, 10, 0))
        .await
        .unwrap();
    assert_eq!(first.id, again.id, "same version is stored once");
    assert_eq!(again.operation_id, None);

    let edited = wallet_messages::insert(
        &mut conn,
        &NewWalletMessage {
            text: "Вы получили 2 USDT",
            ..message(ub, 10, 1_760_000_100)
        },
    )
    .await
    .unwrap();
    assert_ne!(edited.id, first.id, "an edit is a new row");

    // Тот же id сообщения в «Избранном» (свой id вместо id бота) — другая строка.
    let saved = wallet_messages::insert(
        &mut conn,
        &NewWalletMessage {
            bot_peer_id: 7001,
            ..message(ub, 10, 0)
        },
    )
    .await
    .unwrap();
    assert_ne!(saved.id, first.id);

    let rows = sqlx::query(
        "SELECT text, entities, reply_markup, is_outgoing FROM wallet_messages \
         WHERE userbot_id = $1 ORDER BY id",
    )
    .bind(ub)
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get::<String, _>("text"), "Вы получили 1 USDT");
    assert_eq!(rows[1].get::<String, _>("text"), "Вы получили 2 USDT");
    assert_eq!(
        rows[0].get::<serde_json::Value, _>("entities"),
        json!({ "urls": [], "date": 1_760_000_000 })
    );
    assert_eq!(
        rows[0].get::<Option<serde_json::Value>, _>("reply_markup"),
        None
    );
}

/// Сообщение записано до разбора без операции, потом — с ключом операции: привязка
/// появляется; вторая операция чужую привязку не перезаписывает.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn operation_is_attached_on_rerecord_and_never_replaced(pool: PgPool) {
    let ub = userbot(&pool, "ub-cb-1").await;
    let mut conn = pool.acquire().await.unwrap();

    let plain = wallet_messages::insert(&mut conn, &message(ub, 20, 0))
        .await
        .unwrap();
    assert_eq!(plain.operation_id, None);

    let unknown_key = wallet_messages::insert(
        &mut conn,
        &NewWalletMessage {
            operation_key: Some("bal-cb-missing"),
            ..message(ub, 20, 0)
        },
    )
    .await
    .unwrap();
    assert_eq!(unknown_key.id, plain.id);
    assert_eq!(unknown_key.operation_id, None);
    assert!(unknown_key.operation_not_found(Some("bal-cb-missing")));

    let op_a = read_balance_op(&pool, "bal-cb-000001").await;
    let op_b = read_balance_op(&pool, "bal-cb-000002").await;

    let claimed = wallet_messages::insert(
        &mut conn,
        &NewWalletMessage {
            operation_key: Some("bal-cb-000001"),
            ..message(ub, 20, 0)
        },
    )
    .await
    .unwrap();
    assert_eq!(claimed.id, plain.id);
    assert_eq!(claimed.operation_id, Some(op_a));
    assert!(!claimed.claimed_by_other());

    let other = wallet_messages::insert(
        &mut conn,
        &NewWalletMessage {
            operation_key: Some("bal-cb-000002"),
            ..message(ub, 20, 0)
        },
    )
    .await
    .unwrap();
    assert_eq!(other.operation_id, Some(op_a), "first claim is kept");
    assert_eq!(other.requested_operation_id, Some(op_b));
    assert!(other.claimed_by_other());

    // Запись без ключа не снимает привязку.
    let plain_again = wallet_messages::insert(&mut conn, &message(ub, 20, 0))
        .await
        .unwrap();
    assert_eq!(plain_again.operation_id, Some(op_a));

    // Новое сообщение сразу с ключом.
    let fresh = wallet_messages::insert(
        &mut conn,
        &NewWalletMessage {
            operation_key: Some("bal-cb-000002"),
            ..message(ub, 21, 0)
        },
    )
    .await
    .unwrap();
    assert_eq!(fresh.operation_id, Some(op_b));
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn message_of_unknown_userbot_is_rejected(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    assert!(matches!(
        wallet_messages::insert(&mut conn, &message(99, 1, 0)).await,
        Err(StorageError::Db(_))
    ));
}

/// Списки аккаунтов и кошельков для `exch check-config` и `exch run`: по порядку id, без
/// байтов сессии, с привязкой кошелька к метке аккаунта.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn accounts_and_wallets_are_listed(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    assert!(sessions::list(&mut conn).await.unwrap().is_empty());
    assert!(wallets::list(&mut conn).await.unwrap().is_empty());

    let both = userbot(&pool, "ub-both").await;
    let spare = userbot(&pool, "ub-spare").await;
    sessions::save_session(&mut conn, "ub-both", &sample_session(3))
        .await
        .unwrap();
    sessions::set_tg_user(&mut conn, "ub-both", 7001)
        .await
        .unwrap();
    sessions::set_status(&mut conn, "ub-both", UserbotStatus::Active)
        .await
        .unwrap();
    let cb = wallets::ensure_wallet(&mut conn, Platform::CryptoBot, both)
        .await
        .unwrap();
    let xr = wallets::ensure_wallet(&mut conn, Platform::XRocket, both)
        .await
        .unwrap();

    let accounts = sessions::list(&mut conn).await.unwrap();
    assert_eq!(
        accounts
            .iter()
            .map(|a| (
                a.id,
                a.label.as_str(),
                a.status,
                a.has_session,
                a.tg_user_id
            ))
            .collect::<Vec<_>>(),
        vec![
            (both, "ub-both", UserbotStatus::Active, true, Some(7001)),
            (spare, "ub-spare", UserbotStatus::New, false, None),
        ]
    );
    assert_eq!(accounts[0].phone_masked, "+31******01");

    let list = wallets::list(&mut conn).await.unwrap();
    assert_eq!(
        list,
        vec![
            wallets::WalletRow {
                wallet_id: cb,
                platform: Platform::CryptoBot,
                label: "cb".into(),
                is_active: true,
                userbot_id: both,
                userbot_label: "ub-both".into(),
            },
            wallets::WalletRow {
                wallet_id: xr,
                platform: Platform::XRocket,
                label: "xr".into(),
                is_active: true,
                userbot_id: both,
                userbot_label: "ub-both".into(),
            },
        ]
    );
}
