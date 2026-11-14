//! Выбор клиента, который переживает перезапуск бота (`client_prefs`, миграция 0007; шаг 6.3
//! плана): «Простой вид» и «Поддержка» нажата. Клиентский бот держит их в памяти и пишет сюда
//! при каждом изменении; при запуске — читает обратно.

use chrono::{DateTime, Utc};
use sqlx::PgConnection;

use crate::error::StorageError;

/// Клиент выбрал простой вид (`true`) или вернул карточки (`false`).
pub async fn set_plain_view(
    conn: &mut PgConnection,
    user_id: i64,
    plain: bool,
) -> Result<(), StorageError> {
    sqlx::query!(
        r#"
        INSERT INTO client_prefs (user_id, plain_view) VALUES ($1, $2)
        ON CONFLICT (user_id) DO UPDATE SET plain_view = EXCLUDED.plain_view, updated_at = now()
        "#,
        user_id,
        plain
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// «Поддержка» нажата (с заявкой или без): ждём вопрос клиента с этого момента.
pub async fn set_support_wait(
    conn: &mut PgConnection,
    user_id: i64,
    order: Option<&str>,
) -> Result<(), StorageError> {
    sqlx::query!(
        r#"
        INSERT INTO client_prefs (user_id, support_order, support_since) VALUES ($1, $2, now())
        ON CONFLICT (user_id) DO UPDATE SET support_order = EXCLUDED.support_order,
                                            support_since = EXCLUDED.support_since,
                                            updated_at = now()
        "#,
        user_id,
        order
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Вопрос получен (или ждать перестали).
pub async fn clear_support_wait(conn: &mut PgConnection, user_id: i64) -> Result<(), StorageError> {
    sqlx::query!(
        "UPDATE client_prefs SET support_order = NULL, support_since = NULL, updated_at = now() WHERE user_id = $1",
        user_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Всё сохранённое: клиенты с простым видом и ожидания вопроса не старше `since`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Saved {
    pub plain_view: Vec<i64>,
    /// Клиент, заявка, с какого момента ждём.
    pub support_waits: Vec<(i64, Option<String>, DateTime<Utc>)>,
}

pub async fn load(conn: &mut PgConnection, since: DateTime<Utc>) -> Result<Saved, StorageError> {
    let plain_view = sqlx::query_scalar!("SELECT user_id FROM client_prefs WHERE plain_view")
        .fetch_all(&mut *conn)
        .await?;
    let support_waits = sqlx::query!(
        r#"SELECT user_id, support_order, support_since AS "since!" FROM client_prefs
            WHERE support_since IS NOT NULL AND support_since > $1"#,
        since
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|r| (r.user_id, r.support_order, r.since))
    .collect();
    Ok(Saved {
        plain_view,
        support_waits,
    })
}
