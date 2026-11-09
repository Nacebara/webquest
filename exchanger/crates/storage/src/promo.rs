//! Журнал рассылки «🔥 Выгодно» (миграция 0002, решение владельца 9 ноября 2026).
//!
//! Рассылка заводится, когда на направлении включился выгодный курс и рассылка включена; одна
//! за раз. Отправщик берёт клиентов по возрастанию `tg_user_id` с курсора, помечает клиента до
//! отправки (дважды не получит) и пропускает тех, кому писали меньше [`COOLDOWN_HOURS`] часов
//! назад.

use chrono::{DateTime, Utc};
use sqlx::{AssertSqlSafe, PgConnection};

use crate::error::StorageError;

/// Одному клиенту — не чаще раза в столько часов.
pub const COOLDOWN_HOURS: i64 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broadcast {
    pub id: i64,
    pub direction: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub cursor_user: i64,
    pub total: i32,
    pub delivered: i32,
    pub skipped: i32,
    pub failed: i32,
}

type Row = (
    i64,
    String,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    i64,
    i32,
    i32,
    i32,
    i32,
);

fn from_row(r: Row) -> Broadcast {
    Broadcast {
        id: r.0,
        direction: r.1,
        started_at: r.2,
        finished_at: r.3,
        cursor_user: r.4,
        total: r.5,
        delivered: r.6,
        skipped: r.7,
        failed: r.8,
    }
}

const COLUMNS: &str =
    "id, direction, started_at, finished_at, cursor_user, total, delivered, skipped, failed";

/// Клиенты, которым можно писать: без действующего бана.
const RECIPIENTS: &str = "NOT EXISTS (SELECT 1 FROM bans b WHERE b.user_id = u.tg_user_id \
     AND b.lifted_at IS NULL AND (b.expires_at IS NULL OR b.expires_at > now()))";

/// Завести рассылку по направлению. Уже идёт другая — `None` (вторую не заводим).
pub async fn start(conn: &mut PgConnection, direction: &str) -> Result<Option<i64>, StorageError> {
    let total: i64 = sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT count(*) FROM users u WHERE {RECIPIENTS}"
    )))
    .fetch_one(&mut *conn)
    .await?;
    let id = sqlx::query_scalar(
        "INSERT INTO promo_broadcasts (direction, total) VALUES ($1, $2) \
         ON CONFLICT ((finished_at IS NULL)) WHERE finished_at IS NULL DO NOTHING RETURNING id",
    )
    .bind(direction)
    .bind(i32::try_from(total).unwrap_or(i32::MAX))
    .fetch_optional(&mut *conn)
    .await?;
    Ok(id)
}

/// Идущая рассылка.
pub async fn running(conn: &mut PgConnection) -> Result<Option<Broadcast>, StorageError> {
    let row: Option<Row> = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM promo_broadcasts WHERE finished_at IS NULL ORDER BY id LIMIT 1"
    )))
    .fetch_optional(conn)
    .await?;
    Ok(row.map(from_row))
}

/// Последняя рассылка (идущая или законченная) — для «Ребалансировки».
pub async fn last(conn: &mut PgConnection) -> Result<Option<Broadcast>, StorageError> {
    let row: Option<Row> = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM promo_broadcasts ORDER BY id DESC LIMIT 1"
    )))
    .fetch_optional(conn)
    .await?;
    Ok(row.map(from_row))
}

/// Сколько клиентов получат рассылку (без забаненных) — для подтверждения владельцу.
pub async fn recipients(conn: &mut PgConnection) -> Result<i64, StorageError> {
    Ok(sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT count(*) FROM users u WHERE {RECIPIENTS}"
    )))
    .fetch_one(conn)
    .await?)
}

/// Следующий клиент после курсора: id и можно ли ему писать (не писали [`COOLDOWN_HOURS`] ч).
/// Курсор сдвигается, клиенту, которому можно, ставится отметка — всё в одном запросе, до
/// отправки. Клиентов больше нет — `None`.
pub async fn claim_next(
    conn: &mut PgConnection,
    broadcast: i64,
) -> Result<Option<(i64, bool)>, StorageError> {
    let row: Option<(i64, bool)> = sqlx::query_as(AssertSqlSafe(format!(
        "WITH next AS ( \
             SELECT u.tg_user_id AS id, \
                    (u.promo_notified_at IS NULL \
                     OR u.promo_notified_at < now() - make_interval(hours => $2)) AS fresh \
               FROM users u, promo_broadcasts p \
              WHERE p.id = $1 AND p.finished_at IS NULL AND u.tg_user_id > p.cursor_user \
                AND {RECIPIENTS} \
              ORDER BY u.tg_user_id LIMIT 1 FOR UPDATE OF u \
         ), mark AS ( \
             UPDATE users SET promo_notified_at = now() \
              WHERE tg_user_id = (SELECT id FROM next WHERE fresh) \
         ), moved AS ( \
             UPDATE promo_broadcasts SET cursor_user = (SELECT id FROM next), \
                    skipped = skipped + CASE WHEN (SELECT fresh FROM next) THEN 0 ELSE 1 END \
              WHERE id = $1 AND EXISTS (SELECT 1 FROM next) \
         ) \
         SELECT id, fresh FROM next"
    )))
    .bind(broadcast)
    .bind(i32::try_from(COOLDOWN_HOURS).unwrap_or(3))
    .fetch_optional(conn)
    .await?;
    Ok(row)
}

/// Итог отправки одному клиенту.
pub async fn count(
    conn: &mut PgConnection,
    broadcast: i64,
    delivered: bool,
) -> Result<(), StorageError> {
    let column = if delivered { "delivered" } else { "failed" };
    sqlx::query(AssertSqlSafe(format!(
        "UPDATE promo_broadcasts SET {column} = {column} + 1 WHERE id = $1"
    )))
    .bind(broadcast)
    .execute(conn)
    .await?;
    Ok(())
}

/// Закончить рассылку: клиенты кончились или выгодный курс уже выключен.
pub async fn finish(conn: &mut PgConnection, broadcast: i64) -> Result<(), StorageError> {
    sqlx::query(
        "UPDATE promo_broadcasts SET finished_at = now() WHERE id = $1 AND finished_at IS NULL",
    )
    .bind(broadcast)
    .execute(conn)
    .await?;
    Ok(())
}
