//! События для пульта (`alerts`, SPEC §8.1): движок и сверка пишут, ops-бот доставляет.
//!
//! Открытое событие с тем же `dedup_key` не дублируется: растёт счётчик и время последнего,
//! ops-бот правит своё сообщение («×7, последнее 14:05»). `payload` — уже без секретов и с
//! замаскированными ссылками на чеки.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{Connection, PgConnection};

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
    /// Сколько раз критичная тревога повторена, пока её никто не принял.
    pub repeats: i32,
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
    repeats: i32,
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
            repeats: r.repeats,
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
               sent_message_id, acked_by, acked_at, repeats
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
               sent_message_id, acked_by, acked_at, repeats
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
               sent_message_id, acked_by, acked_at, repeats
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

/// Закрыть событие по ключу без человека (причина ушла: резерв выровнялся). Принятое человеком,
/// но ещё не ушедшее состояние тоже помечается ушедшим: следующий подъём — новая тревога.
/// `true` — была открытая тревога и она закрыта.
pub async fn resolve(conn: &mut PgConnection, dedup_key: &str) -> Result<bool, StorageError> {
    let n = sqlx::query!(
        "UPDATE alerts SET acked_at = now(), resolved_at = now() WHERE dedup_key = $1 AND acked_at IS NULL",
        dedup_key
    )
    .execute(&mut *conn)
    .await?
    .rows_affected();
    sqlx::query!(
        "UPDATE alerts SET resolved_at = now() WHERE dedup_key = $1 AND resolved_at IS NULL",
        dedup_key
    )
    .execute(&mut *conn)
    .await?;
    Ok(n == 1)
}

/// Поднять тревогу-состояние (недостача, вход сброшен, движок не работает): то же, что
/// [`raise`], но если человек её уже принял, а причина не ушла ([`resolve`] не звали), — тихо
/// обновить принятую (счётчик, данные), без новой тревоги со звуком (шаг 2.8, AUDIT-UI П1).
pub async fn raise_condition(
    conn: &mut PgConnection,
    a: &NewAlert<'_>,
) -> Result<i64, StorageError> {
    // Решение «принята — тихо обновить, нет — поднять» — под блокировкой последней строки
    // ключа: «Принял», нажатый между проверкой и вставкой, иначе поднимал принятую тревогу
    // заново со звуком (та же гонка, что у `ensure_condition`, найдена 16.11). `FOR UPDATE`
    // ждёт незавершённое «Принял» и видит его итог.
    let mut tx = conn.begin().await?;
    let last = sqlx::query!(
        "SELECT id, acked_by, resolved_at FROM alerts WHERE dedup_key = $1 ORDER BY id DESC LIMIT 1 FOR UPDATE",
        a.dedup_key
    )
    .fetch_optional(&mut *tx)
    .await?;
    let id = match last {
        Some(row) if row.acked_by.is_some() && row.resolved_at.is_none() => {
            sqlx::query!(
                "UPDATE alerts SET count = count + 1, last_at = now(), payload = $2 WHERE id = $1",
                row.id,
                a.payload,
            )
            .execute(&mut *tx)
            .await?;
            row.id
        }
        _ => raise(&mut tx, a).await?,
    };
    tx.commit().await?;
    Ok(id)
}

/// [`ensure`] для состояния, которое проверяется по кругу (заявка на разборе): принятое
/// человеком не открывается заново, пока причина не ушла.
pub async fn ensure_condition(
    conn: &mut PgConnection,
    a: &NewAlert<'_>,
) -> Result<bool, StorageError> {
    // Проверка «по ключу уже есть нерешённая тревога — принятая или нет» и вставка — одним
    // запросом. Раньше это были два запроса: «Принял», нажатый между ними, делал прежнюю
    // тревогу принятой, частичный уникальный индекс (`WHERE acked_at IS NULL`) переставал её
    // видеть — и принятая тревога заводилась заново (гонка, найдена 16.11 под нагрузкой тестов).
    let n = sqlx::query!(
        r#"
        INSERT INTO alerts (level, topic, dedup_key, title, payload)
        SELECT $1::text, $2::text, $3::text, $4::text, $5::jsonb
         WHERE NOT EXISTS (SELECT 1 FROM alerts WHERE dedup_key = $3 AND resolved_at IS NULL)
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

/// Копия тревоги у сотрудника: его чат и сообщение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Copy {
    pub chat_id: i64,
    /// 0 — сообщения нет (отметка «позвал админа» без копии).
    pub message_id: i32,
    pub sent_at: DateTime<Utc>,
    /// Сотрудник позвал админа (П-3): его копию не правим и не повторяем.
    pub muted_at: Option<DateTime<Utc>>,
}

/// Копии тревоги у сотрудников.
pub async fn copies(conn: &mut PgConnection, alert_id: i64) -> Result<Vec<Copy>, StorageError> {
    Ok(sqlx::query_as!(
        Copy,
        "SELECT chat_id, message_id, sent_at, muted_at FROM alert_copies WHERE alert_id = $1 ORDER BY chat_id",
        alert_id
    )
    .fetch_all(conn)
    .await?)
}

/// Сообщение `message_id` в чате `chat_id` — копия тревоги (П-5: переход из тревоги — новым
/// сообщением, тревога остаётся тревогой).
pub async fn is_copy(
    conn: &mut PgConnection,
    chat_id: i64,
    message_id: i32,
) -> Result<bool, StorageError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM alert_copies WHERE chat_id = $1 AND message_id = $2) AS "copy!""#,
        chat_id,
        message_id
    )
    .fetch_one(conn)
    .await?)
}

/// Сотрудник позвал админа (П-3): повторы тревоги ему больше не приходят.
pub async fn mute_copy(
    conn: &mut PgConnection,
    alert_id: i64,
    chat_id: i64,
) -> Result<(), StorageError> {
    sqlx::query!(
        r#"
        INSERT INTO alert_copies (alert_id, chat_id, message_id, muted_at) VALUES ($1, $2, 0, now())
        ON CONFLICT (alert_id, chat_id) DO UPDATE SET muted_at = COALESCE(alert_copies.muted_at, now())
        "#,
        alert_id,
        chat_id,
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Запомнить копию: новое сообщение (или новое вместо прежнего при повторе).
pub async fn save_copy(
    conn: &mut PgConnection,
    alert_id: i64,
    chat_id: i64,
    message_id: i32,
) -> Result<(), StorageError> {
    sqlx::query!(
        r#"
        INSERT INTO alert_copies (alert_id, chat_id, message_id) VALUES ($1, $2, $3)
        ON CONFLICT (alert_id, chat_id) DO UPDATE SET message_id = EXCLUDED.message_id, sent_at = now()
        "#,
        alert_id,
        chat_id,
        message_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Критичные тревоги, которые никто не принял, а последняя копия старше `every`: пора повторить.
pub async fn due_repeats(
    conn: &mut PgConnection,
    every: std::time::Duration,
    limit: i64,
) -> Result<Vec<AlertRow>, StorageError> {
    let secs = i64::try_from(every.as_secs()).unwrap_or(i64::MAX);
    let rows = sqlx::query_as!(
        Raw,
        r#"
        SELECT a.id, a.level, a.topic, a.dedup_key, a.title, a.payload, a.count, a.first_at,
               a.last_at, a.sent_message_id, a.acked_by, a.acked_at, a.repeats
          FROM alerts a
         WHERE a.acked_at IS NULL AND a.level = 'critical'
           AND (SELECT max(c.sent_at) FROM alert_copies c WHERE c.alert_id = a.id)
               < now() - $1::bigint * interval '1 second'
         ORDER BY a.id
         LIMIT $2
        "#,
        secs,
        limit
    )
    .fetch_all(conn)
    .await?;
    rows.into_iter().map(TryInto::try_into).collect()
}

/// Критичная тревога повторена ещё раз.
/// Засчитать повтор тревоги — только если её ещё никто не принял (одним запросом: «Принял»
/// не проскочит между проверкой и повтором). `None` — уже принята, повтор не нужен.
pub async fn bump_repeat(conn: &mut PgConnection, id: i64) -> Result<Option<i32>, StorageError> {
    Ok(sqlx::query_scalar!(
        "UPDATE alerts SET repeats = repeats + 1 WHERE id = $1 AND acked_at IS NULL RETURNING repeats",
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// Последнее событие с ключом — открытое или уже закрытое (П-8: «Ответить» на отвеченное
/// обращение показывает, кто и что ответил).
pub async fn latest_by_key(
    conn: &mut PgConnection,
    dedup_key: &str,
) -> Result<Option<AlertRow>, StorageError> {
    let row = sqlx::query_as!(
        Raw,
        r#"
        SELECT id, level, topic, dedup_key, title, payload, count, first_at, last_at,
               sent_message_id, acked_by, acked_at, repeats
          FROM alerts WHERE dedup_key = $1 ORDER BY id DESC LIMIT 1
        "#,
        dedup_key
    )
    .fetch_optional(conn)
    .await?;
    row.map(TryInto::try_into).transpose()
}

/// Дописать поля в данные события (кто ответил на обращение и что).
pub async fn merge_payload(
    conn: &mut PgConnection,
    id: i64,
    patch: &Value,
) -> Result<(), StorageError> {
    sqlx::query!(
        "UPDATE alerts SET payload = payload || $2 WHERE id = $1",
        id,
        patch
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Открытое событие по ключу (`balance_mismatch:xrocket` …).
pub async fn open_by_key(
    conn: &mut PgConnection,
    dedup_key: &str,
) -> Result<Option<AlertRow>, StorageError> {
    let row = sqlx::query_as!(
        Raw,
        r#"
        SELECT id, level, topic, dedup_key, title, payload, count, first_at, last_at,
               sent_message_id, acked_by, acked_at, repeats
          FROM alerts WHERE dedup_key = $1 AND acked_at IS NULL
        "#,
        dedup_key
    )
    .fetch_optional(conn)
    .await?;
    row.map(TryInto::try_into).transpose()
}
