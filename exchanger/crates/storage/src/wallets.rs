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

/// Кошелёк сервиса и аккаунт юзербота, чей это баланс.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletRow {
    pub wallet_id: i16,
    pub platform: Platform,
    /// `wallet_accounts.label`: `cb`, `xr`, `cb-reserve`…
    pub label: String,
    pub is_active: bool,
    pub userbot_id: i16,
    pub userbot_label: String,
}

/// Все кошельки с их аккаунтами по порядку id (`exch check-config`, запуск `exch run`).
pub async fn list(conn: &mut PgConnection) -> Result<Vec<WalletRow>, StorageError> {
    let rows = sqlx::query!(
        r#"
        SELECT w.id, w.platform, w.label, w.is_active, w.userbot_id, u.label AS userbot_label
        FROM wallet_accounts w JOIN userbot_accounts u ON u.id = w.userbot_id
        ORDER BY w.id
        "#,
    )
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|r| {
            Ok(WalletRow {
                wallet_id: r.id,
                platform: Platform::from_db_name(&r.platform).ok_or_else(|| {
                    StorageError::corrupt("wallet_accounts.platform", &r.platform)
                })?,
                label: r.label,
                is_active: r.is_active,
                userbot_id: r.userbot_id,
                userbot_label: r.userbot_label,
            })
        })
        .collect()
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

/// Активный кошелёк платформы: (id кошелька, id аккаунта юзербота).
pub async fn active(
    conn: &mut PgConnection,
    platform: Platform,
) -> Result<Option<(i16, i16)>, StorageError> {
    let row = sqlx::query!(
        "SELECT id, userbot_id FROM wallet_accounts WHERE platform = $1 AND is_active",
        platform.db_name()
    )
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|r| (r.id, r.userbot_id)))
}
