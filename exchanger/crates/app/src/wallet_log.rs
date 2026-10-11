//! Журнал сообщений ботов кошельков (CLAUDE.md п. 8): [`userbot::MessageLog`] поверх
//! [`storage::wallet_messages`]. Сценарии юзербота зовут его до разбора каждого сообщения.
//!
//! Колонки (`storage::wallet_messages`):
//! - `bot_peer_id` — закреплённый id бота (`mtproto::config`), для «Избранного» — свой user id
//!   аккаунта (`userbot_accounts.tg_user_id`, после подключения — id из сессии);
//! - `entities` — `{"urls": [...], "date": <unix>, "via_bot": "cryptobot"|"xrocket"}`;
//! - `reply_markup` — кнопки по рядам, как в `RawMessage::buttons`; `NULL` без кнопок;
//! - `operation_id` — по ключу операции (`operations.idempotency_key`).
//!
//! Журнал не должен мешать деньгам: ошибка БД или зависшая БД — предупреждение в лог, а не
//! ошибка сценария. Паники нет ни на одном пути. Текст сообщений в лог не пишется: в нём
//! бывают ссылки на чеки (CLAUDE.md п. 16).

use std::collections::HashMap;
use std::sync::{PoisonError, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use mtproto::PinnedBots;
use serde_json::{Value, json};
use storage::PgPool;
use storage::wallet_messages::{self, NewWalletMessage, StoredMessage};
use userbot::{Chat, MessageLog, RawMessage};

/// Сколько ждать БД на одно сообщение, прежде чем отпустить сценарий.
pub const DEFAULT_RECORD_TIMEOUT: Duration = Duration::from_secs(3);

/// Что журнал знает об аккаунте.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountIds {
    /// `userbot_accounts.id`.
    pub userbot_id: i16,
    /// Свой user id Telegram: `bot_peer_id` сообщений «Избранного».
    pub self_id: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum WalletLogError {
    #[error("userbot account {0} is not registered")]
    UnknownAccount(String),
    #[error("own Telegram id of {0} is unknown: Saved Messages cannot be journaled")]
    NoSelfId(String),
    #[error("message field out of range: {0}")]
    OutOfRange(&'static str),
    #[error(transparent)]
    Storage(#[from] storage::StorageError),
    /// Нет соединения из пула.
    #[error("database pool: {0}")]
    Pool(#[from] sqlx::Error),
}

/// Id чата для колонки `bot_peer_id`.
pub fn peer_id(chat: Chat, bots: &PinnedBots, self_id: Option<i64>) -> Option<i64> {
    match chat {
        Chat::WalletBot(platform) => Some(bots.get(platform).id),
        Chat::SavedMessages => self_id,
    }
}

/// `entities`: URL сущностей, время отправки и бот инлайн-режима.
pub fn entities_json(msg: &RawMessage) -> Value {
    let mut v = json!({ "urls": msg.entity_urls, "date": msg.date });
    if let (Some(via), Value::Object(map)) = (msg.via_bot, &mut v) {
        map.insert("via_bot".into(), Value::String(via.db_name().to_owned()));
    }
    v
}

/// `reply_markup`: кнопки по рядам; `None` без кнопок.
pub fn markup_json(msg: &RawMessage) -> Option<Value> {
    if msg.buttons.iter().all(Vec::is_empty) {
        return None;
    }
    serde_json::to_value(&msg.buttons).ok()
}

/// Строка `wallet_messages` из сообщения.
pub fn to_row<'a>(
    userbot_id: i16,
    bot_peer_id: i64,
    msg: &'a RawMessage,
    op: Option<&'a str>,
) -> Result<NewWalletMessage<'a>, WalletLogError> {
    let edit_date = match msg.edit_date {
        None => 0,
        Some(d) => i32::try_from(d).map_err(|_| WalletLogError::OutOfRange("edit_date"))?,
    };
    Ok(NewWalletMessage {
        userbot_id,
        bot_peer_id,
        message_id: msg.id,
        edit_date,
        is_outgoing: msg.out,
        text: &msg.text,
        entities: Some(entities_json(msg)),
        reply_markup: markup_json(msg),
        operation_key: op,
    })
}

/// Журнал в PostgreSQL. Один на процесс, общий для всех аккаунтов.
pub struct WalletLog {
    pool: PgPool,
    bots: PinnedBots,
    accounts: RwLock<HashMap<String, AccountIds>>,
    timeout: Duration,
}

impl std::fmt::Debug for WalletLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalletLog")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl WalletLog {
    pub fn new(pool: PgPool, bots: PinnedBots) -> Self {
        Self {
            pool,
            bots,
            accounts: RwLock::new(HashMap::new()),
            timeout: DEFAULT_RECORD_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Запомнить аккаунт (после подключения: `self_id` — id из сессии). Без регистрации
    /// журнал найдёт аккаунт в БД по метке при первом сообщении.
    pub fn register(&self, label: &str, ids: AccountIds) {
        self.accounts
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(label.to_owned(), ids);
    }

    fn cached(&self, label: &str) -> Option<AccountIds> {
        self.accounts
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(label)
            .copied()
    }

    async fn ids(&self, label: &str) -> Result<AccountIds, WalletLogError> {
        if let Some(ids) = self.cached(label) {
            return Ok(ids);
        }
        let mut conn = self.pool.acquire().await?;
        let account = storage::sessions::find(&mut conn, label)
            .await?
            .ok_or_else(|| WalletLogError::UnknownAccount(label.to_owned()))?;
        let ids = AccountIds {
            userbot_id: account.id,
            self_id: account.tg_user_id,
        };
        self.register(label, ids);
        Ok(ids)
    }

    /// Записать сообщение и вернуть итог записи (ошибки — наружу; для тестов и `exch record`).
    pub async fn try_record(
        &self,
        account: &str,
        msg: &RawMessage,
        op: Option<&str>,
    ) -> Result<StoredMessage, WalletLogError> {
        let ids = self.ids(account).await?;
        let peer = peer_id(msg.chat, &self.bots, ids.self_id)
            .ok_or_else(|| WalletLogError::NoSelfId(account.to_owned()))?;
        let row = to_row(ids.userbot_id, peer, msg, op)?;
        let mut conn = self.pool.acquire().await?;
        Ok(wallet_messages::insert(&mut conn, &row).await?)
    }
}

#[async_trait]
impl MessageLog for WalletLog {
    async fn record(&self, account: &str, msg: &RawMessage, op: Option<&str>) {
        let chat = msg.chat;
        match tokio::time::timeout(self.timeout, self.try_record(account, msg, op)).await {
            Ok(Ok(stored)) => {
                if stored.operation_not_found(op) {
                    tracing::warn!(
                        account,
                        ?chat,
                        msg_id = msg.id,
                        op,
                        "wallet message names an operation that does not exist"
                    );
                }
                if stored.claimed_by_other() {
                    tracing::warn!(
                        account,
                        ?chat,
                        msg_id = msg.id,
                        op,
                        row = stored.id,
                        "wallet message already belongs to another operation"
                    );
                }
            }
            Ok(Err(e)) => {
                tracing::warn!(account, ?chat, msg_id = msg.id, op, error = %e, "cannot journal wallet message");
            }
            Err(_) => {
                tracing::warn!(
                    account,
                    ?chat,
                    msg_id = msg.id,
                    op,
                    timeout_ms = self.timeout.as_millis() as u64,
                    "journaling wallet message timed out"
                );
            }
        }
    }
}
