//! Журнал сообщений ботов кошельков (`wallet_messages`, CLAUDE.md п. 8): каждое сообщение
//! сохраняется до разбора. Ключ — `(userbot_id, bot_peer_id, message_id, edit_date)`: правка
//! сообщения — новая строка, повторная запись той же версии — та же строка.
//!
//! Что лежит в колонках (заполняет `app::wallet_log` из `userbot::RawMessage`):
//! - `bot_peer_id` — закреплённый id бота (`1559501630` @send, `5014831088` @xrocket); для
//!   «Избранного» — **свой** user id аккаунта (`userbot_accounts.tg_user_id`): там лежат
//!   инлайн-чеки, созданные через бота, и метки операций. Id сообщений личных чатов — общий
//!   счётчик аккаунта, поэтому ключ не пересекается с чатами ботов;
//! - `entities` — `{"urls": [...], "via_bot": "cryptobot"|"xrocket", "date": <unix>}`: URL
//!   сущностей, бот инлайн-режима и время отправки (отдельных колонок под них в схеме нет);
//! - `reply_markup` — кнопки по рядам (`[[{"text": …, "kind": …}]]`), `NULL` без кнопок;
//! - `operation_id` — операция, которая признала сообщение своим (по `idempotency_key`).

use sqlx::PgConnection;

use crate::error::StorageError;

/// Сообщение для записи.
#[derive(Debug, Clone, PartialEq)]
pub struct NewWalletMessage<'a> {
    pub userbot_id: i16,
    /// Id бота кошелька или свой id для «Избранного».
    pub bot_peer_id: i64,
    pub message_id: i32,
    /// 0 — исходная версия, иначе unix-время правки.
    pub edit_date: i32,
    pub is_outgoing: bool,
    pub text: &'a str,
    pub entities: Option<serde_json::Value>,
    pub reply_markup: Option<serde_json::Value>,
    /// `operations.idempotency_key` операции, которая признала сообщение своим.
    pub operation_key: Option<&'a str>,
}

/// Итог записи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredMessage {
    pub id: i64,
    /// Операция, к которой сообщение привязано после записи.
    pub operation_id: Option<i64>,
    /// Операция, найденная по `operation_key` (`None` — ключа не было или такой операции нет).
    pub requested_operation_id: Option<i64>,
}

impl StoredMessage {
    /// Ключ операции дан, но такой операции в БД нет.
    pub fn operation_not_found(&self, key: Option<&str>) -> bool {
        key.is_some() && self.requested_operation_id.is_none()
    }

    /// Сообщение уже принадлежит другой операции: первая привязка не перезаписывается.
    pub fn claimed_by_other(&self) -> bool {
        matches!(
            (self.requested_operation_id, self.operation_id),
            (Some(want), Some(have)) if want != have
        )
    }
}

/// Записать сообщение (upsert). Повторная запись той же версии ничего не меняет, кроме
/// привязки к операции: если строка ещё ни к чему не привязана, а ключ операции дан и
/// операция есть, — `operation_id` заполняется. Существующая привязка не меняется.
pub async fn insert(
    conn: &mut PgConnection,
    m: &NewWalletMessage<'_>,
) -> Result<StoredMessage, StorageError> {
    let requested_operation_id = match m.operation_key {
        Some(key) => {
            sqlx::query_scalar!("SELECT id FROM operations WHERE idempotency_key = $1", key)
                .fetch_optional(&mut *conn)
                .await?
        }
        None => None,
    };
    let row = sqlx::query!(
        r#"
        INSERT INTO wallet_messages AS m
            (userbot_id, bot_peer_id, message_id, edit_date, is_outgoing, text,
             entities, reply_markup, operation_id)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        ON CONFLICT (userbot_id, bot_peer_id, message_id, edit_date)
        DO UPDATE SET operation_id = COALESCE(m.operation_id, EXCLUDED.operation_id)
        RETURNING m.id, m.operation_id
        "#,
        m.userbot_id,
        m.bot_peer_id,
        m.message_id,
        m.edit_date,
        m.is_outgoing,
        m.text,
        m.entities,
        m.reply_markup,
        requested_operation_id,
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok(StoredMessage {
        id: row.id,
        operation_id: row.operation_id,
        requested_operation_id,
    })
}
