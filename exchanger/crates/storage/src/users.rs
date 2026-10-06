//! Клиенты бота.

use sqlx::PgConnection;

use crate::error::StorageError;

/// Профиль из апдейта Telegram.
#[derive(Debug, Clone, Default)]
pub struct Profile<'a> {
    pub username: Option<&'a str>,
    pub first_name: Option<&'a str>,
    pub language_code: Option<&'a str>,
}

/// Создать пользователя или обновить профиль и `last_seen_at`.
pub async fn upsert(
    conn: &mut PgConnection,
    tg_user_id: i64,
    profile: &Profile<'_>,
) -> Result<(), StorageError> {
    sqlx::query!(
        r#"
        INSERT INTO users (tg_user_id, username, first_name, language_code)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (tg_user_id) DO UPDATE
           SET username = EXCLUDED.username,
               first_name = EXCLUDED.first_name,
               language_code = EXCLUDED.language_code,
               last_seen_at = now()
        "#,
        tg_user_id,
        profile.username,
        profile.first_name,
        profile.language_code,
    )
    .execute(conn)
    .await?;
    Ok(())
}
