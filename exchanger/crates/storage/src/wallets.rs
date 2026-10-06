//! Аккаунты юзерботов и четыре кошелька (SPEC §3.1). Наполнение — при подключении
//! аккаунтов (веха M3); здесь — идемпотентное создание записей.

use domain::{WalletKind, WalletRef};
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

/// Кошелёк по метке `cb:app` и т. п. Для личного кошелька обязателен аккаунт юзербота.
pub async fn ensure_wallet(
    conn: &mut PgConnection,
    wallet: WalletRef,
    userbot_id: Option<i16>,
) -> Result<i16, StorageError> {
    debug_assert_eq!(wallet.kind == WalletKind::Personal, userbot_id.is_some());
    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO wallet_accounts (platform, kind, label, userbot_id) VALUES ($1, $2, $3, $4)
        ON CONFLICT (label) DO UPDATE SET userbot_id = EXCLUDED.userbot_id
        RETURNING id
        "#,
        wallet.platform.db_name(),
        wallet.kind.db_name(),
        wallet.label(),
        userbot_id,
    )
    .fetch_one(conn)
    .await?;
    Ok(id)
}
