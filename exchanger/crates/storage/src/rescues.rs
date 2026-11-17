//! «Спасти резерв» (миграция 0013): чек на весь баланс кошелька юзербота — владельцу.

use chrono::{DateTime, Utc};
use domain::{Asset, Decimal, Platform};
use sqlx::PgConnection;

use crate::StorageError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RescueStatus {
    Pending,
    Issued,
    Unknown,
    Failed,
}

impl RescueStatus {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            RescueStatus::Pending => "pending",
            RescueStatus::Issued => "issued",
            RescueStatus::Unknown => "unknown",
            RescueStatus::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Result<Self, StorageError> {
        Ok(match s {
            "pending" => RescueStatus::Pending,
            "issued" => RescueStatus::Issued,
            "unknown" => RescueStatus::Unknown,
            "failed" => RescueStatus::Failed,
            other => return Err(StorageError::corrupt("rescues.status", other)),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rescue {
    pub id: i64,
    pub platform: Platform,
    pub wallet_account_id: i16,
    pub asset: Asset,
    pub amount: Decimal,
    pub measured: Decimal,
    pub status: RescueStatus,
    pub check_url: Option<String>,
    pub requested_by: i64,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

struct Raw {
    id: i64,
    platform: String,
    wallet_account_id: i16,
    asset: String,
    amount: Decimal,
    measured: Decimal,
    status: String,
    check_url: Option<String>,
    requested_by: i64,
    reason: Option<String>,
    created_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
}

impl TryFrom<Raw> for Rescue {
    type Error = StorageError;

    fn try_from(r: Raw) -> Result<Self, StorageError> {
        Ok(Self {
            id: r.id,
            platform: Platform::from_db_name(&r.platform)
                .ok_or_else(|| StorageError::corrupt("rescues.platform", &r.platform))?,
            wallet_account_id: r.wallet_account_id,
            asset: r
                .asset
                .parse()
                .map_err(|_| StorageError::corrupt("rescues.asset", &r.asset))?,
            amount: r.amount,
            measured: r.measured,
            status: RescueStatus::parse(&r.status)?,
            check_url: r.check_url,
            requested_by: r.requested_by,
            reason: r.reason,
            created_at: r.created_at,
            finished_at: r.finished_at,
        })
    }
}

/// Новое спасение. На кошелёк — одно незавершённое (`rescues_one_open_per_wallet`):
/// второе — [`StorageError::RescueOpen`].
#[allow(clippy::too_many_arguments)]
pub async fn insert(
    conn: &mut PgConnection,
    platform: Platform,
    wallet_account_id: i16,
    asset: Asset,
    amount: Decimal,
    measured: Decimal,
    requested_by: i64,
    reason: Option<&str>,
) -> Result<i64, StorageError> {
    sqlx::query_scalar!(
        r#"
        INSERT INTO rescues (platform, wallet_account_id, asset, amount, measured, requested_by, reason)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id
        "#,
        platform.db_name(),
        wallet_account_id,
        asset.code(),
        amount,
        measured,
        requested_by,
        reason,
    )
    .fetch_one(conn)
    .await
    .map_err(|e| match crate::error::unique_violation(&e) {
        Some("rescues_one_open_per_wallet") => StorageError::RescueOpen,
        _ => StorageError::Db(e),
    })
}

pub async fn get(conn: &mut PgConnection, id: i64, lock: bool) -> Result<Rescue, StorageError> {
    let raw = if lock {
        sqlx::query_as!(
            Raw,
            r#"SELECT id, platform, wallet_account_id, asset, amount, measured, status, check_url,
                      requested_by, reason, created_at, finished_at
                 FROM rescues WHERE id = $1 FOR UPDATE"#,
            id
        )
        .fetch_one(conn)
        .await?
    } else {
        sqlx::query_as!(
            Raw,
            r#"SELECT id, platform, wallet_account_id, asset, amount, measured, status, check_url,
                      requested_by, reason, created_at, finished_at
                 FROM rescues WHERE id = $1"#,
            id
        )
        .fetch_one(conn)
        .await?
    };
    raw.try_into()
}

/// Последние спасения, новые первыми.
pub async fn recent(conn: &mut PgConnection, limit: i64) -> Result<Vec<Rescue>, StorageError> {
    let rows = sqlx::query_as!(
        Raw,
        r#"SELECT id, platform, wallet_account_id, asset, amount, measured, status, check_url,
                  requested_by, reason, created_at, finished_at
             FROM rescues ORDER BY id DESC LIMIT $1"#,
        limit
    )
    .fetch_all(conn)
    .await?;
    rows.into_iter().map(Rescue::try_from).collect()
}

/// Итог спасения. Только из незавершённого состояния: повторная обработка ответа не
/// перепишет итог — [`StorageError::RescueState`].
pub async fn finish(
    conn: &mut PgConnection,
    id: i64,
    status: RescueStatus,
    check_url: Option<&str>,
) -> Result<(), StorageError> {
    let done = !matches!(status, RescueStatus::Pending | RescueStatus::Unknown);
    let n = sqlx::query!(
        r#"
        UPDATE rescues
           SET status = $2, check_url = COALESCE($3, check_url),
               finished_at = CASE WHEN $4 THEN now() ELSE finished_at END
         WHERE id = $1 AND status IN ('pending', 'unknown')
        "#,
        id,
        status.as_db_str(),
        check_url,
        done,
    )
    .execute(conn)
    .await?
    .rows_affected();
    if n == 1 {
        Ok(())
    } else {
        Err(StorageError::RescueState(id))
    }
}
