//! Аккаунты юзерботов и их кошельки: по одному активному на платформу (SPEC v0.2 §3.1).
//! Наполнение — при подключении аккаунтов (`exch login`); здесь — идемпотентное создание.

use domain::{Platform, wallet_label};
use sqlx::PgConnection;

use crate::error::StorageError;

/// Запись аккаунта юзербота без сессии (сессия добавляется командой `exch login`).
pub async fn ensure_userbot(
    conn: &mut PgConnection,
    label: &str,
    phone_masked: &str,
) -> Result<i16, StorageError> {
    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO userbot_accounts (label, phone_masked) VALUES ($1, $2)
        ON CONFLICT (label) DO UPDATE SET phone_masked = EXCLUDED.phone_masked
        RETURNING id
        "#,
        label,
        phone_masked,
    )
    .fetch_one(conn)
    .await?;
    Ok(id)
}

/// К какому аккаунту юзербота привязан кошелёк платформы (метка `cb` / `xr`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletBinding {
    pub wallet_id: i16,
    pub userbot_id: i16,
    pub userbot_label: String,
}

/// Привязка кошелька платформы, если кошелёк уже есть.
pub async fn wallet_binding(
    conn: &mut PgConnection,
    platform: Platform,
) -> Result<Option<WalletBinding>, StorageError> {
    let row = sqlx::query_as!(
        WalletBinding,
        r#"
        SELECT w.id AS wallet_id, w.userbot_id, u.label AS userbot_label
        FROM wallet_accounts w JOIN userbot_accounts u ON u.id = w.userbot_id
        WHERE w.label = $1
        "#,
        wallet_label(platform),
    )
    .fetch_optional(conn)
    .await?;
    Ok(row)
}

/// Активный кошелёк платформы (метка `cb` / `xr`) — баланс аккаунта `userbot_id`.
pub async fn ensure_wallet(
    conn: &mut PgConnection,
    platform: Platform,
    userbot_id: i16,
) -> Result<i16, StorageError> {
    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO wallet_accounts (platform, label, userbot_id) VALUES ($1, $2, $3)
        ON CONFLICT (label) DO UPDATE SET userbot_id = EXCLUDED.userbot_id
        RETURNING id
        "#,
        platform.db_name(),
        wallet_label(platform),
        userbot_id,
    )
    .fetch_one(conn)
    .await?;
    Ok(id)
}
