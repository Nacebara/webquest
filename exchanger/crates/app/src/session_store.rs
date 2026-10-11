//! Хранилище сессии MTProto в `userbot_accounts` (HANDOVER §5, M2.2): адаптер
//! [`mtproto::SessionStore`] поверх [`storage::sessions`].
//!
//! Сюда приходят и отсюда уходят только зашифрованные байты ([`SealedSession`]): шифрует и
//! расшифровывает крейт `mtproto` ключом `SESSION_KEY`. Метка аккаунта — ключ строки в БД и
//! часть AAD шифра, поэтому сессию одной метки нельзя подставить другой.

use async_trait::async_trait;
use mtproto::{SealedSession, SessionStore, StoreError};
use secrecy::ExposeSecret;
use storage::PgPool;
use storage::sessions::{self, StoredSession};

use crate::config;

/// Номер текущего ключа `SESSION_KEY` (`userbot_accounts.key_version`). Ротация ключа — новый
/// номер и повторный `exch login` (сессия, зашифрованная старым ключом, не расшифруется).
pub const SESSION_KEY_VERSION: i16 = 1;

/// Ключ шифрования сессий для `mtproto` из ключа конфигурации.
pub fn session_key(key: &config::SessionKey) -> mtproto::SessionKey {
    mtproto::SessionKey::from_bytes(SESSION_KEY_VERSION, key.secret().expose_secret())
}

/// Строка БД из зашифрованной сессии.
pub fn to_stored(sealed: &SealedSession) -> StoredSession {
    StoredSession {
        ciphertext: sealed.ciphertext.clone(),
        nonce: sealed.nonce.to_vec(),
        key_version: sealed.key_version,
    }
}

/// Зашифрованная сессия из строки БД.
pub fn from_stored(stored: StoredSession) -> Result<SealedSession, mtproto::CryptoError> {
    SealedSession::from_parts(stored.key_version, &stored.nonce, stored.ciphertext)
}

/// Сессия аккаунта `label` в PostgreSQL. Запись аккаунта должна существовать (её создаёт
/// `exch login`); сохранение в неизвестную метку — ошибка, а не новая строка.
#[derive(Debug, Clone)]
pub struct PgSessionStore {
    pool: PgPool,
    label: String,
}

impl PgSessionStore {
    pub fn new(pool: PgPool, label: impl Into<String>) -> Self {
        Self {
            pool,
            label: label.into(),
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

fn store_error(label: &str, what: &str, e: impl std::fmt::Display) -> StoreError {
    // В тексте ошибок storage и sqlx нет параметров запроса, то есть нет байтов сессии.
    StoreError(format!("{what} session of {label}: {e}"))
}

#[async_trait]
impl SessionStore for PgSessionStore {
    async fn load(&self) -> Result<Option<SealedSession>, StoreError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| store_error(&self.label, "load", e))?;
        let stored = sessions::load_session(&mut conn, &self.label)
            .await
            .map_err(|e| store_error(&self.label, "load", e))?;
        stored
            .map(from_stored)
            .transpose()
            .map_err(|e| store_error(&self.label, "load", e))
    }

    async fn save(&self, sealed: &SealedSession) -> Result<(), StoreError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| store_error(&self.label, "save", e))?;
        sessions::save_session(&mut conn, &self.label, &to_stored(sealed))
            .await
            .map_err(|e| store_error(&self.label, "save", e))
    }
}
