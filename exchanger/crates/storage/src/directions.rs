//! Направления обмена и их настройки (`directions`, SPEC §2.3).

use domain::fees::{FeeMode, FeeSchedule, FeeTier};
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
    /// Запас на движение курса у счёта в фиате, % (SPEC П5): сервис доплачивает до него сам.
    pub fiat_buffer_pct: Decimal,
    pub is_open: bool,
    pub auto_paused: bool,
    pub pause_reason: Option<String>,
    /// Фикс или ступени ([`domain::fees`]).
    pub fee_mode: FeeMode,
    /// Ступени; хранятся и в режиме фикса. `None` — не заданы.
    pub fee_tiers: Option<FeeSchedule>,
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
               new_user_max_amount, fiat_buffer_pct, is_open, auto_paused, pause_reason,
               fee_mode, fee_tiers
          FROM directions WHERE code = $1
        "#,
        direction.code()
    )
    .fetch_one(conn)
    .await?;
    let fee_mode = FeeMode::from_db_str(&r.fee_mode)
        .ok_or_else(|| StorageError::corrupt("directions.fee_mode", &r.fee_mode))?;
    let tiers: Vec<FeeTier> = serde_json::from_value(r.fee_tiers.clone())
        .map_err(|_| StorageError::corrupt("directions.fee_tiers", &r.fee_tiers))?;
    let fee_tiers = if tiers.is_empty() {
        None
    } else {
        Some(
            FeeSchedule::new(tiers)
                .map_err(|_| StorageError::corrupt("directions.fee_tiers", &r.fee_tiers))?,
        )
    };
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
        fiat_buffer_pct: r.fiat_buffer_pct,
        is_open: r.is_open,
        auto_paused: r.auto_paused,
        pause_reason: r.pause_reason,
        fee_mode,
        fee_tiers,
    })
}

/// Все направления в порядке показа (основной спрос — первым, DESIGN-v0.2 Р1).
pub async fn all(conn: &mut PgConnection) -> Result<Vec<DirectionRow>, StorageError> {
    let mut out = Vec::with_capacity(Direction::ALL.len());
    for direction in [
        Direction::CbToXrCheck,
        Direction::XrToCbCheck,
        Direction::PayCbInvoice,
        Direction::PayXrInvoice,
    ] {
        out.push(get(&mut *conn, direction).await?);
    }
    Ok(out)
}

/// Записать настраиваемые поля направления (пульт). Проверки схемы (пол ≤ ставка ≤ потолок,
/// мин ≤ макс, лимит новичка ≤ макс) — последняя линия; пульт проверяет раньше.
pub async fn save_terms(
    conn: &mut PgConnection,
    row: &DirectionRow,
    staff: i64,
) -> Result<(), StorageError> {
    let tiers = serde_json::to_value(row.fee_tiers.as_ref().map_or(&[][..], |t| t.tiers()))
        .map_err(|e| StorageError::corrupt("directions.fee_tiers", e.to_string()))?;
    sqlx::query!(
        r#"
        UPDATE directions
           SET fee_pct = $2, min_fee = $3, skew_k_pct = $4, fee_floor_pct = $5, fee_cap_pct = $6,
               min_amount = $7, max_amount = $8, new_user_max_amount = $9, fiat_buffer_pct = $10,
               fee_mode = $11, fee_tiers = $12, updated_by = $13, updated_at = now()
         WHERE code = $1
        "#,
        row.direction.code(),
        row.fee_pct,
        row.min_fee,
        row.skew_k_pct,
        row.fee_floor_pct,
        row.fee_cap_pct,
        row.min_amount,
        row.max_amount,
        row.new_user_max_amount,
        row.fiat_buffer_pct,
        row.fee_mode.as_db_str(),
        tiers,
        staff,
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Снять автопаузу (человек разобрался с причиной).
pub async fn clear_auto_pause(
    conn: &mut PgConnection,
    direction: Direction,
    staff: i64,
) -> Result<(), StorageError> {
    sqlx::query!(
        r#"
        UPDATE directions SET auto_paused = FALSE, pause_reason = NULL, updated_by = $2,
                              updated_at = now()
         WHERE code = $1
        "#,
        direction.code(),
        staff
    )
    .execute(conn)
    .await?;
    Ok(())
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

/// Сторона направления, с которой связан кошелёк платформы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Платформа, где принимаем чек клиента.
    Source,
    /// Платформа, где выплачиваем (создаём чек или оплачиваем счёт).
    Target,
}

/// Автопауза всех направлений, у которых `platform` стоит на стороне `side`. Возвращает,
/// сколько направлений встало на паузу.
pub async fn auto_pause_platform(
    conn: &mut PgConnection,
    platform: domain::Platform,
    side: Side,
    reason: &str,
) -> Result<u64, StorageError> {
    let column_is_source = side == Side::Source;
    let n = sqlx::query!(
        r#"
        UPDATE directions SET auto_paused = TRUE, pause_reason = $3, updated_at = now()
         WHERE (CASE WHEN $2 THEN source_platform ELSE target_platform END) = $1
           AND NOT auto_paused
        "#,
        platform.db_name(),
        column_is_source,
        reason
    )
    .execute(conn)
    .await?
    .rows_affected();
    Ok(n)
}

/// Снять автопаузу с направлений платформы, поставленную ровно с этой причиной (другие
/// причины не трогаются). Сколько направлений снято.
pub async fn clear_auto_pause_reason(
    conn: &mut PgConnection,
    platform: domain::Platform,
    reason: &str,
) -> Result<u64, StorageError> {
    let n = sqlx::query!(
        r#"
        UPDATE directions SET auto_paused = FALSE, pause_reason = NULL, updated_at = now()
         WHERE auto_paused AND pause_reason = $2
           AND (source_platform = $1 OR target_platform = $1)
        "#,
        platform.db_name(),
        reason
    )
    .execute(conn)
    .await?
    .rows_affected();
    Ok(n)
}

/// Минимальная сумма чека актива на платформе (`platform_assets.min_check`).
pub async fn min_check(
    conn: &mut PgConnection,
    platform: domain::Platform,
    asset: domain::Asset,
) -> Result<Decimal, StorageError> {
    let min = sqlx::query_scalar!(
        "SELECT min_check FROM platform_assets WHERE platform = $1 AND asset = $2",
        platform.db_name(),
        asset.code()
    )
    .fetch_one(conn)
    .await?;
    Ok(min)
}
