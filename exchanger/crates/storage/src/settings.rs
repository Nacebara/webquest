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

/// Значение флага вида `{"on": …}`. Флаг со сроком — `{"on": true, "until": "<RFC 3339>"}`
/// («Обслуживание 30 мин», П-13): срок вышел — флаг выключен, хотя запись ещё «вкл». Так
/// обслуживание кончается само для всех читателей сразу, без фоновой задачи.
fn on_of(v: &Value) -> Option<bool> {
    let on = v.get("on").and_then(Value::as_bool)?;
    Some(on && until_of(v).is_none_or(|until| until > Utc::now()))
}

fn until_of(v: &Value) -> Option<DateTime<Utc>> {
    v.get("until")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
}

/// Включён ли флаг вида `{"on": true}` (`maintenance`, `kill_switch_payouts`); срок — [`on_of`].
pub async fn flag_on(conn: &mut PgConnection, key: &str) -> Result<bool, StorageError> {
    Ok(get(conn, key)
        .await?
        .and_then(|v| on_of(&v))
        .unwrap_or(false))
}

/// Флаг вида `{"on": …}` со значением по умолчанию, если настройки нет или она без `on`.
pub async fn flag(conn: &mut PgConnection, key: &str, default: bool) -> Result<bool, StorageError> {
    Ok(get(conn, key)
        .await?
        .and_then(|v| on_of(&v))
        .unwrap_or(default))
}

/// До какого времени включён флаг со сроком (экран «Обслуживание · до 15:30», клиенту —
/// «Обновляемся до 15:30»). Выключен, срок вышел или флаг без срока — `None`.
pub async fn on_until(
    conn: &mut PgConnection,
    key: &str,
) -> Result<Option<DateTime<Utc>>, StorageError> {
    Ok(get(conn, key)
        .await?
        .filter(|v| on_of(v) == Some(true))
        .and_then(|v| until_of(&v)))
}

/// С какого времени флаг вида `{"on": true}` включён: `since` в значении (продление срока его не
/// сдвигает), иначе время последней записи настройки. Флаг выключен, срок вышел или настройки
/// нет — `None` (главный экран пульта: «Обслуживание · с 14:20»).
pub async fn on_since(
    conn: &mut PgConnection,
    key: &str,
) -> Result<Option<DateTime<Utc>>, StorageError> {
    let row = sqlx::query!("SELECT value, updated_at FROM settings WHERE key = $1", key)
        .fetch_optional(conn)
        .await?;
    Ok(row.filter(|r| on_of(&r.value) == Some(true)).map(|r| {
        r.value
            .get("since")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map_or(r.updated_at, |d| d.with_timezone(&Utc))
    }))
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
