//! Сессия MTProto в `userbot_accounts` через `app::session_store` на живом PostgreSQL:
//! в БД только шифротекст, чужой ключ не расшифровывает и ничего не перезаписывает.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use app::session_store::{self, PgSessionStore, SESSION_KEY_VERSION};
use async_trait::async_trait;
use mtproto::session::{EncryptedSession, SessionError, SessionSnapshot, StoredUser};
use mtproto::{
    ConnectError, GrammersTransport, LoginError, LoginPrompt, MtprotoConfig, SessionKey,
    SessionStore,
};
use secrecy::SecretString;
use sqlx::{PgPool, Row};

const LABEL: &str = "ub-1";

fn key(fill: u8) -> SessionKey {
    SessionKey::from_bytes(SESSION_KEY_VERSION, &[fill; 32])
}

/// Сессия «после входа»: свой аккаунт 7001 и auth key домашнего DC.
fn logged_in_session() -> EncryptedSession {
    let mut snap = SessionSnapshot::default();
    let home = snap.home_dc;
    if let Some(dc) = snap.dc_options.get_mut(&home) {
        dc.auth_key = Some([0x5A; 256]);
    }
    let session = EncryptedSession::new(snap, []);
    session
        .remember_user(StoredUser {
            id: 7001,
            access_hash: Some(99),
            bot: Some(false),
            is_self: Some(true),
        })
        .unwrap();
    session
}

async fn account(pool: &PgPool, label: &str) {
    let mut conn = pool.acquire().await.unwrap();
    storage::wallets::ensure_userbot(&mut conn, label, "+31*******42")
        .await
        .unwrap();
}

async fn raw_row(pool: &PgPool, label: &str) -> (Option<Vec<u8>>, Option<Vec<u8>>, i16) {
    let row = sqlx::query(
        "SELECT session_ciphertext, session_nonce, key_version FROM userbot_accounts WHERE label = $1",
    )
    .bind(label)
    .fetch_one(pool)
    .await
    .unwrap();
    (row.get(0), row.get(1), row.get(2))
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn save_then_load_round_trips_only_ciphertext(pool: PgPool) {
    account(&pool, LABEL).await;
    let store = PgSessionStore::new(pool.clone(), LABEL);
    assert_eq!(store.load().await.unwrap(), None, "no login yet");

    let k = key(7);
    let session = logged_in_session();
    let sealed = session.seal(&k, LABEL).unwrap();
    store.save(&sealed).await.unwrap();

    let loaded = store.load().await.unwrap().expect("saved session");
    assert_eq!(loaded, sealed);
    let reopened = EncryptedSession::open(&k, LABEL, &loaded, []).unwrap();
    assert_eq!(reopened.self_user().map(|u| u.id), Some(7001));
    assert_eq!(
        reopened.snapshot().unwrap(),
        session.snapshot().unwrap(),
        "same session after the round trip"
    );

    // В колонках — шифротекст: открытого снимка (магия формата `EXSD`, auth key) там нет.
    let (ciphertext, nonce, version) = raw_row(&pool, LABEL).await;
    let ciphertext = ciphertext.unwrap();
    let plain = session.snapshot().unwrap().encode().unwrap();
    assert_ne!(ciphertext.as_slice(), plain.as_slice());
    assert!(!ciphertext.windows(4).any(|w| w == b"EXSD"));
    assert!(!ciphertext.windows(16).any(|w| w == [0x5A; 16]));
    assert_eq!(nonce.unwrap().len(), mtproto::crypto::NONCE_LEN);
    assert_eq!(version, SESSION_KEY_VERSION);

    // Повторное сохранение (SessionPersister после апдейтов) заменяет запись: новый nonce.
    let again = session.seal(&k, LABEL).unwrap();
    store.save(&again).await.unwrap();
    let reloaded = store.load().await.unwrap().unwrap();
    assert_eq!(reloaded, again);
    assert_ne!(reloaded.nonce, sealed.nonce);
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn session_of_one_label_does_not_open_under_another(pool: PgPool) {
    account(&pool, "ub-a").await;
    account(&pool, "ub-b").await;
    let k = key(7);
    let sealed = logged_in_session().seal(&k, "ub-a").unwrap();
    PgSessionStore::new(pool.clone(), "ub-a")
        .save(&sealed)
        .await
        .unwrap();
    // Кто-то скопировал байты в другую метку — AAD с меткой не даёт их открыть.
    PgSessionStore::new(pool.clone(), "ub-b")
        .save(&sealed)
        .await
        .unwrap();
    let copied = PgSessionStore::new(pool.clone(), "ub-b")
        .load()
        .await
        .unwrap()
        .unwrap();
    assert!(EncryptedSession::open(&k, "ub-b", &copied, []).is_err());
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn saving_to_unknown_label_fails(pool: PgPool) {
    let store = PgSessionStore::new(pool.clone(), "ub-nobody");
    let sealed = logged_in_session().seal(&key(1), "ub-nobody").unwrap();
    let err = store.save(&sealed).await.unwrap_err();
    assert!(err.to_string().contains("ub-nobody"), "{err}");
    assert_eq!(store.load().await.unwrap(), None);
}

struct NoInput;

#[async_trait]
impl LoginPrompt for NoInput {
    async fn phone(&mut self) -> Result<SecretString, LoginError> {
        Err(LoginError::Prompt("no input in tests".into()))
    }
    async fn code(&mut self, _: u32) -> Result<SecretString, LoginError> {
        Err(LoginError::Prompt("no input in tests".into()))
    }
    async fn password(&mut self, _: Option<&str>, _: u32) -> Result<SecretString, LoginError> {
        Err(LoginError::Prompt("no input in tests".into()))
    }
    async fn notice(&mut self, _: &str) {}
}

/// Неверный `SESSION_KEY`: и вход, и подключение отказывают до сети, а сохранённая сессия
/// остаётся байт в байт прежней.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn wrong_key_fails_without_overwriting(pool: PgPool) {
    account(&pool, LABEL).await;
    let right = key(7);
    let sealed = logged_in_session().seal(&right, LABEL).unwrap();
    PgSessionStore::new(pool.clone(), LABEL)
        .save(&sealed)
        .await
        .unwrap();
    let before = raw_row(&pool, LABEL).await;

    let cfg = MtprotoConfig::new(LABEL, 12345, SecretString::from("api-hash"));
    let store: Arc<dyn SessionStore> = Arc::new(PgSessionStore::new(pool.clone(), LABEL));
    let err = mtproto::login(&cfg, &key(8), Arc::clone(&store), &mut NoInput)
        .await
        .unwrap_err();
    assert!(
        matches!(err, LoginError::Session(SessionError::Crypto(_))),
        "{err:?}"
    );

    let err = GrammersTransport::connect(cfg.clone(), key(8), Arc::clone(&store))
        .await
        .err()
        .expect("wrong key must not connect");
    assert!(matches!(err, ConnectError::Session(_)), "{err:?}");

    assert_eq!(raw_row(&pool, LABEL).await, before, "nothing overwritten");
    let still = session_store::from_stored(
        storage::sessions::load_session(&mut pool.acquire().await.unwrap(), LABEL)
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(EncryptedSession::open(&right, LABEL, &still, []).is_ok());
}

/// Нет сессии — подключение просит `exch login`, в сеть не идёт.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn connect_without_session_asks_for_login(pool: PgPool) {
    account(&pool, LABEL).await;
    let cfg = MtprotoConfig::new(LABEL, 12345, SecretString::from("api-hash"));
    let store: Arc<dyn SessionStore> = Arc::new(PgSessionStore::new(pool.clone(), LABEL));
    let err = GrammersTransport::connect(cfg, key(7), store)
        .await
        .err()
        .expect("no session");
    assert!(matches!(err, ConnectError::NoSession), "{err:?}");
}

#[test]
fn store_debug_shows_label_only() {
    let stored = session_store::to_stored(&mtproto::seal(&key(1), LABEL, b"secret-bytes").unwrap());
    let dbg = format!("{stored:?}");
    assert!(dbg.contains("ciphertext_len"), "{dbg}");
    assert!(!dbg.contains("secret"), "{dbg}");
    let back = session_store::from_stored(stored).unwrap();
    assert_eq!(
        mtproto::open(&key(1), LABEL, &back).unwrap().as_slice(),
        b"secret-bytes"
    );
}
