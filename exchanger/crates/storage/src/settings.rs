//! Настройки сервиса (`settings`): JSON по ключу.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgConnection;

use crate::error::StorageError;

pub async fn get(conn: &mut PgConnection, key: &str) -> Result<Option<Value>, StorageError> {
    let value = sqlx::query_scalar!("SELECT value FROM settings WHERE key = $1", key)
        .fetch_optional(conn)
        .await?;
    Ok(value)
}

/// Включён ли флаг вида `{"on": true}` (`maintenance`, `kill_switch_payouts`).
pub async fn flag_on(conn: &mut PgConnection, key: &str) -> Result<bool, StorageError> {
    Ok(get(conn, key)
        .await?
        .and_then(|v| v.get("on").and_then(Value::as_bool))
        .unwrap_or(false))
}

/// Флаг вида `{"on": …}` со значением по умолчанию, если настройки нет или она без `on`.
pub async fn flag(conn: &mut PgConnection, key: &str, default: bool) -> Result<bool, StorageError> {
    Ok(get(conn, key)
        .await?
        .and_then(|v| v.get("on").and_then(Value::as_bool))
        .unwrap_or(default))
}

/// С какого времени флаг вида `{"on": true}` включён: время последней записи настройки. Флаг
/// выключен или настройки нет — `None` (главный экран пульта: «Обслуживание · с 14:20»).
pub async fn on_since(
    conn: &mut PgConnection,
    key: &str,
) -> Result<Option<DateTime<Utc>>, StorageError> {
    let since = sqlx::query_scalar!(
        r#"SELECT updated_at FROM settings WHERE key = $1 AND value->>'on' = 'true'"#,
        key
    )
    .fetch_optional(conn)
    .await?;
    Ok(since)
}

pub async fn set(
    conn: &mut PgConnection,
    key: &str,
    value: &Value,
    staff: Option<i64>,
) -> Result<(), StorageError> {
    sqlx::query!(
        r#"
        INSERT INTO settings (key, value, updated_by) VALUES ($1, $2, $3)
        ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_by = EXCLUDED.updated_by,
                                        updated_at = now()
        "#,
        key,
        value,
        staff
    )
    .execute(conn)
    .await?;
    Ok(())
}
