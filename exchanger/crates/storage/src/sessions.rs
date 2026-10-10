//! Аккаунты юзерботов (`userbot_accounts`): зашифрованная сессия MTProto, статус, свой user id.
//!
//! Здесь только байты: шифротекст, nonce и номер ключа. Шифрование и формат сессии — крейт
//! `mtproto`; адаптер `mtproto::SessionStore` поверх этих функций — `app::session_store`
//! (storage не зависит от mtproto, см. «Карта крейтов» в CLAUDE.md). Открытый текст сессии
//! и полный телефон в БД не попадают: телефон — только маской (`mtproto::mask_phone`).

use sqlx::PgConnection;

use crate::error::{StorageError, unique_violation};

/// Зашифрованная сессия, как она лежит в `userbot_accounts`. `Debug` печатает только длины.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredSession {
    /// `session_ciphertext`: шифротекст вместе с тегом.
    pub ciphertext: Vec<u8>,
    /// `session_nonce`.
    pub nonce: Vec<u8>,
    /// `key_version`: каким ключом `SESSION_KEY` зашифровано.
    pub key_version: i16,
}

impl std::fmt::Debug for StoredSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredSession")
            .field("key_version", &self.key_version)
            .field("ciphertext_len", &self.ciphertext.len())
            .finish_non_exhaustive()
    }
}

/// `userbot_accounts.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UserbotStatus {
    /// Запись есть, входа ещё не было.
    New,
    Active,
    FloodWait,
    /// Сессию отозвали или она больше не авторизована: нужен `exch login`.
    LoggedOut,
    Banned,
    /// Выключен человеком.
    Disabled,
}

impl UserbotStatus {
    pub const ALL: [UserbotStatus; 6] = [
        UserbotStatus::New,
        UserbotStatus::Active,
        UserbotStatus::FloodWait,
        UserbotStatus::LoggedOut,
        UserbotStatus::Banned,
        UserbotStatus::Disabled,
    ];

    pub const fn db_name(self) -> &'static str {
        match self {
            UserbotStatus::New => "new",
            UserbotStatus::Active => "active",
            UserbotStatus::FloodWait => "flood_wait",
            UserbotStatus::LoggedOut => "logged_out",
            UserbotStatus::Banned => "banned",
            UserbotStatus::Disabled => "disabled",
        }
    }

    pub fn from_db_name(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.db_name() == s)
    }
}

/// Запись аккаунта без сессии.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserbotAccount {
    pub id: i16,
    pub label: String,
    pub tg_user_id: Option<i64>,
    pub phone_masked: String,
    pub status: UserbotStatus,
    pub has_session: bool,
}

/// Аккаунт по метке.
pub async fn find(
    conn: &mut PgConnection,
    label: &str,
) -> Result<Option<UserbotAccount>, StorageError> {
    let row = sqlx::query!(
        r#"
        SELECT id, label, tg_user_id, phone_masked, status,
               session_ciphertext IS NOT NULL AS "has_session!"
        FROM userbot_accounts WHERE label = $1
        "#,
        label,
    )
    .fetch_optional(conn)
    .await?;
    row.map(|r| {
        Ok(UserbotAccount {
            id: r.id,
            label: r.label,
            tg_user_id: r.tg_user_id,
            phone_masked: r.phone_masked,
            status: UserbotStatus::from_db_name(&r.status)
                .ok_or_else(|| StorageError::corrupt("userbot_accounts.status", &r.status))?,
            has_session: r.has_session,
        })
    })
    .transpose()
}

/// Зашифрованная сессия аккаунта. `None` — нет аккаунта с такой меткой или входа ещё не было.
pub async fn load_session(
    conn: &mut PgConnection,
    label: &str,
) -> Result<Option<StoredSession>, StorageError> {
    let row = sqlx::query!(
        r#"
        SELECT session_ciphertext, session_nonce, key_version
        FROM userbot_accounts WHERE label = $1
        "#,
        label,
    )
    .fetch_optional(conn)
    .await?;
    Ok(
        row.and_then(|r| match (r.session_ciphertext, r.session_nonce) {
            (Some(ciphertext), Some(nonce)) => Some(StoredSession {
                ciphertext,
                nonce,
                key_version: r.key_version,
            }),
            // CHECK в схеме не даёт одному быть без другого.
            _ => None,
        }),
    )
}

/// Записать зашифрованную сессию существующего аккаунта (запись создаёт `exch login`
/// через `wallets::ensure_userbot`).
pub async fn save_session(
    conn: &mut PgConnection,
    label: &str,
    session: &StoredSession,
) -> Result<(), StorageError> {
    let updated = sqlx::query!(
        r#"
        UPDATE userbot_accounts
        SET session_ciphertext = $2, session_nonce = $3, key_version = $4
        WHERE label = $1
        "#,
        label,
        session.ciphertext,
        session.nonce,
        session.key_version,
    )
    .execute(conn)
    .await?
    .rows_affected();
    if updated == 0 {
        return Err(StorageError::UnknownUserbot(label.to_owned()));
    }
    Ok(())
}

/// Сменить статус. `active` заодно отмечает `last_ok_at`.
pub async fn set_status(
    conn: &mut PgConnection,
    label: &str,
    status: UserbotStatus,
) -> Result<(), StorageError> {
    let updated = sqlx::query!(
        r#"
        UPDATE userbot_accounts
        SET status = $2,
            last_ok_at = CASE WHEN $2 = 'active' THEN now() ELSE last_ok_at END
        WHERE label = $1
        "#,
        label,
        status.db_name(),
    )
    .execute(conn)
    .await?
    .rows_affected();
    if updated == 0 {
        return Err(StorageError::UnknownUserbot(label.to_owned()));
    }
    Ok(())
}

/// Записать user id Telegram аккаунта (после входа). Один аккаунт Telegram — одна метка.
pub async fn set_tg_user(
    conn: &mut PgConnection,
    label: &str,
    tg_user_id: i64,
) -> Result<(), StorageError> {
    let result = sqlx::query!(
        "UPDATE userbot_accounts SET tg_user_id = $2 WHERE label = $1",
        label,
        tg_user_id,
    )
    .execute(conn)
    .await;
    match result {
        Ok(done) if done.rows_affected() == 0 => {
            Err(StorageError::UnknownUserbot(label.to_owned()))
        }
        Ok(_) => Ok(()),
        Err(e) if unique_violation(&e) == Some("userbot_accounts_tg_user_id_key") => {
            Err(StorageError::TelegramAccountInUse(tg_user_id))
        }
        Err(e) => Err(e.into()),
    }
}
