//! События для пульта (`alerts`, SPEC §8.1): движок и сверка пишут, ops-бот доставляет.
//!
//! Открытое событие с тем же `dedup_key` не дублируется: растёт счётчик и время последнего,
//! ops-бот правит своё сообщение («×7, последнее 14:05»). `payload` — уже без секретов и с
//! замаскированными ссылками на чеки.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgConnection;

use crate::error::StorageError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Info,
    Notice,
    Warn,
    Critical,
}

impl Level {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Notice => "notice",
            Level::Warn => "warn",
            Level::Critical => "critical",
        }
    }

    pub fn from_db_str(s: &str) -> Option<Self> {
        [Level::Info, Level::Notice, Level::Warn, Level::Critical]
            .into_iter()
            .find(|l| l.as_db_str() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Topic {
    Exchanges,
    Errors,
    Reserves,
    Security,
    Admin,
    Digest,
}

impl Topic {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Topic::Exchanges => "exchanges",
            Topic::Errors => "errors",
            Topic::Reserves => "reserves",
            Topic::Security => "security",
            Topic::Admin => "admin",
            Topic::Digest => "digest",
        }
    }

    pub fn from_db_str(s: &str) -> Option<Self> {
        [
            Topic::Exchanges,
            Topic::Errors,
            Topic::Reserves,
            Topic::Security,
            Topic::Admin,
            Topic::Digest,
        ]
        .into_iter()
        .find(|t| t.as_db_str() == s)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewAlert<'a> {
    pub level: Level,
    pub topic: Topic,
    /// `manual_review:15`, `promo_hint:xr_to_cb_check` …
    pub dedup_key: &'a str,
    /// Вид события для пульта: `manual_review`, `promo_hint`, `promo_switched` …
    pub title: &'a str,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlertRow {
    pub id: i64,
    pub level: Level,
    pub topic: Topic,
    pub dedup_key: String,
    pub title: String,
    pub payload: Value,
    pub count: i32,
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    pub sent_message_id: Option<i32>,
    pub acked_by: Option<i64>,
    pub acked_at: Option<DateTime<Utc>>,
}

/// Поднять событие: открытое с тем же ключом — `count + 1`, новые данные, сообщение пульта
/// будет поправлено (`sent_message_id` сбрасывается в «нужно обновить» через `last_at`).
pub async fn raise(conn: &mut PgConnection, a: &NewAlert<'_>) -> Result<i64, StorageError> {
    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO alerts (level, topic, dedup_key, title, payload)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (dedup_key) WHERE acked_at IS NULL DO UPDATE
           SET count = alerts.count + 1, last_at = now(), payload = EXCLUDED.payload,
               level = CASE
                   WHEN array_position(ARRAY['info', 'notice', 'warn', 'critical'], EXCLUDED.level)
                      > array_position(ARRAY['info', 'notice', 'warn', 'critical'], alerts.level)
                   THEN EXCLUDED.level ELSE alerts.level END
        RETURNING id
        "#,
        a.level.as_db_str(),
        a.topic.as_db_str(),
        a.dedup_key,
        a.title,
        a.payload,
    )
    .fetch_one(conn)
    .await?;
    Ok(id)
}

/// Открыть событие, если открытого с этим ключом ещё нет (без счётчика повторов): для
/// состояний, которые проверяются по кругу (заявка на разборе). `true` — открыто сейчас.
pub async fn ensure(conn: &mut PgConnection, a: &NewAlert<'_>) -> Result<bool, StorageError> {
    let n = sqlx::query!(
        r#"
        INSERT INTO alerts (level, topic, dedup_key, title, payload)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (dedup_key) WHERE acked_at IS NULL DO NOTHING
        "#,
        a.level.as_db_str(),
        a.topic.as_db_str(),
        a.dedup_key,
        a.title,
        a.payload,
    )
    .execute(conn)
    .await?
    .rows_affected();
    Ok(n == 1)
}

struct Raw {
    id: i64,
    level: String,
    topic: String,
    dedup_key: String,
    title: String,
    payload: Value,
    count: i32,
    first_at: DateTime<Utc>,
    last_at: DateTime<Utc>,
    sent_message_id: Option<i32>,
    acked_by: Option<i64>,
    acked_at: Option<DateTime<Utc>>,
}

impl TryFrom<Raw> for AlertRow {
    type Error = StorageError;

    fn try_from(r: Raw) -> Result<Self, StorageError> {
        Ok(Self {
            id: r.id,
            level: Level::from_db_str(&r.level)
                .ok_or_else(|| StorageError::corrupt("alerts.level", &r.level))?,
            topic: Topic::from_db_str(&r.topic)
                .ok_or_else(|| StorageError::corrupt("alerts.topic", &r.topic))?,
            dedup_key: r.dedup_key,
            title: r.title,
            payload: r.payload,
            count: r.count,
            first_at: r.first_at,
            last_at: r.last_at,
            sent_message_id: r.sent_message_id,
            acked_by: r.acked_by,
            acked_at: r.acked_at,
        })
    }
}

/// Открытые события, которые пульт ещё не показал или которые изменились после показа
/// (`delivered_at` — когда пульт последний раз отправил или поправил сообщение).
pub async fn pending(conn: &mut PgConnection, limit: i64) -> Result<Vec<AlertRow>, StorageError> {
    let rows = sqlx::query_as!(
        Raw,
        r#"
        SELECT id, level, topic, dedup_key, title, payload, count, first_at, last_at,
               sent_message_id, acked_by, acked_at
          FROM alerts
         WHERE acked_at IS NULL AND (delivered_at IS NULL OR last_at > delivered_at)
         ORDER BY id
         LIMIT $1
        "#,
        limit
    )
    .fetch_all(conn)
    .await?;
    rows.into_iter().map(TryInto::try_into).collect()
}

/// Открытые события (для `/dash`): сначала важные.
pub async fn open(conn: &mut PgConnection, limit: i64) -> Result<Vec<AlertRow>, StorageError> {
    let rows = sqlx::query_as!(
        Raw,
        r#"
        SELECT id, level, topic, dedup_key, title, payload, count, first_at, last_at,
               sent_message_id, acked_by, acked_at
          FROM alerts
         WHERE acked_at IS NULL
         ORDER BY CASE level WHEN 'critical' THEN 0 WHEN 'warn' THEN 1 WHEN 'notice' THEN 2 ELSE 3 END,
                  last_at DESC
         LIMIT $1
        "#,
        limit
    )
    .fetch_all(conn)
    .await?;
    rows.into_iter().map(TryInto::try_into).collect()
}

pub async fn get(conn: &mut PgConnection, id: i64) -> Result<Option<AlertRow>, StorageError> {
    let row = sqlx::query_as!(
        Raw,
        r#"
        SELECT id, level, topic, dedup_key, title, payload, count, first_at, last_at,
               sent_message_id, acked_by, acked_at
          FROM alerts WHERE id = $1
        "#,
        id
    )
    .fetch_optional(conn)
    .await?;
    row.map(TryInto::try_into).transpose()
}

/// Пульт показал событие (отправил или поправил сообщение). `message_id` — `None`, если
/// показывать некому (в пульте никого): событие не будет висеть в очереди доставки.
pub async fn mark_delivered(
    conn: &mut PgConnection,
    id: i64,
    message_id: Option<i32>,
) -> Result<(), StorageError> {
    sqlx::query!(
        r#"
        UPDATE alerts SET sent_message_id = COALESCE($2, sent_message_id), delivered_at = now()
         WHERE id = $1
        "#,
        id,
        message_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// «Принял»: событие закрыто, следующее с тем же ключом откроется заново.
pub async fn ack(conn: &mut PgConnection, id: i64, staff: i64) -> Result<bool, StorageError> {
    let n = sqlx::query!(
        "UPDATE alerts SET acked_by = $2, acked_at = now() WHERE id = $1 AND acked_at IS NULL",
        id,
        staff
    )
    .execute(conn)
    .await?
    .rows_affected();
    Ok(n == 1)
}

/// Закрыть открытое событие по ключу без человека (причина ушла: резерв выровнялся).
pub async fn resolve(conn: &mut PgConnection, dedup_key: &str) -> Result<bool, StorageError> {
    let n = sqlx::query!(
        "UPDATE alerts SET acked_at = now() WHERE dedup_key = $1 AND acked_at IS NULL",
        dedup_key
    )
    .execute(conn)
    .await?
    .rows_affected();
    Ok(n == 1)
}
