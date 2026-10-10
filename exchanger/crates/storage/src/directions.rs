//! Направления обмена и их настройки (`directions`, SPEC §2.3).

use domain::{Decimal, Direction};
use sqlx::PgConnection;

use crate::error::StorageError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectionRow {
    pub direction: Direction,
    pub fee_pct: Decimal,
    pub min_fee: Decimal,
    pub skew_k_pct: Decimal,
    pub fee_floor_pct: Decimal,
    pub fee_cap_pct: Decimal,
    pub min_amount: Decimal,
    pub max_amount: Decimal,
    pub new_user_max_amount: Decimal,
    pub is_open: bool,
    pub auto_paused: bool,
    pub pause_reason: Option<String>,
}

impl DirectionRow {
    /// Принимает ли направление новые заявки.
    pub fn accepting(&self) -> bool {
        self.is_open && !self.auto_paused
    }
}

pub async fn get(
    conn: &mut PgConnection,
    direction: Direction,
) -> Result<DirectionRow, StorageError> {
    let r = sqlx::query!(
        r#"
        SELECT fee_pct, min_fee, skew_k_pct, fee_floor_pct, fee_cap_pct, min_amount, max_amount,
               new_user_max_amount, is_open, auto_paused, pause_reason
          FROM directions WHERE code = $1
        "#,
        direction.code()
    )
    .fetch_one(conn)
    .await?;
    Ok(DirectionRow {
        direction,
        fee_pct: r.fee_pct,
        min_fee: r.min_fee,
        skew_k_pct: r.skew_k_pct,
        fee_floor_pct: r.fee_floor_pct,
        fee_cap_pct: r.fee_cap_pct,
        min_amount: r.min_amount,
        max_amount: r.max_amount,
        new_user_max_amount: r.new_user_max_amount,
        is_open: r.is_open,
        auto_paused: r.auto_paused,
        pause_reason: r.pause_reason,
    })
}

/// Открыть или закрыть направление (владелец).
pub async fn set_open(
    conn: &mut PgConnection,
    direction: Direction,
    open: bool,
) -> Result<(), StorageError> {
    sqlx::query!(
        "UPDATE directions SET is_open = $2, updated_at = now() WHERE code = $1",
        direction.code(),
        open
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Автопауза системой (резерв, незнакомый ответ, сверка) с причиной.
pub async fn auto_pause(
    conn: &mut PgConnection,
    direction: Direction,
    reason: &str,
) -> Result<(), StorageError> {
    sqlx::query!(
        "UPDATE directions SET auto_paused = TRUE, pause_reason = $2, updated_at = now() WHERE code = $1",
        direction.code(),
        reason
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Шаг округления выплаты актива на платформе (`platform_assets.payout_step`).
pub async fn payout_step(
    conn: &mut PgConnection,
    platform: domain::Platform,
    asset: domain::Asset,
) -> Result<Decimal, StorageError> {
    let step = sqlx::query_scalar!(
        "SELECT payout_step FROM platform_assets WHERE platform = $1 AND asset = $2",
        platform.db_name(),
        asset.code()
    )
    .fetch_one(conn)
    .await?;
    Ok(step)
}

/// Целевой резерв кошелька по активу (`reserve_thresholds.target`), если задан.
pub async fn reserve_target(
    conn: &mut PgConnection,
    wallet_account_id: i16,
    asset: domain::Asset,
) -> Result<Option<Decimal>, StorageError> {
    let target = sqlx::query_scalar!(
        "SELECT target FROM reserve_thresholds WHERE wallet_account_id = $1 AND asset = $2",
        wallet_account_id,
        asset.code()
    )
    .fetch_optional(conn)
    .await?;
    Ok(target)
}
