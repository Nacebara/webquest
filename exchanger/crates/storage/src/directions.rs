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

/// Снять автопаузу (человек разобрался с причиной): все причины разом.
pub async fn clear_auto_pause(
    conn: &mut PgConnection,
    direction: Direction,
    staff: i64,
) -> Result<(), StorageError> {
    sqlx::query!(
        "DELETE FROM direction_pauses WHERE direction = $1",
        direction.code()
    )
    .execute(&mut *conn)
    .await?;
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

/// Направления, которые сотрудник закрыл кнопкой и с тех пор не открыл, — по журналу действий,
/// по порядку закрытия. Закрытые с самого начала (их ещё ни разу не открывали) сюда не входят:
/// это не пауза, а направление, с которым сервис не работает (главный экран пульта, П3).
pub async fn closed_by_staff(conn: &mut PgConnection) -> Result<Vec<Direction>, StorageError> {
    let codes = sqlx::query_scalar!(
        r#"
        SELECT d.code AS "code!"
        FROM directions d
        JOIN LATERAL (
            SELECT action, at FROM admin_audit
            WHERE target_type = 'direction' AND target_id = d.code
              AND action IN ('direction.open', 'direction.close')
            ORDER BY at DESC, id DESC
            LIMIT 1
        ) last ON last.action = 'direction.close'
        WHERE NOT d.is_open
        ORDER BY last.at
        "#
    )
    .fetch_all(conn)
    .await?;
    Ok(codes.iter().filter_map(|c| c.parse().ok()).collect())
}

/// Автопауза одного направления системой, без привязки к кошельку.
pub async fn auto_pause(
    conn: &mut PgConnection,
    direction: Direction,
    reason: &str,
) -> Result<(), StorageError> {
    add_pause(conn, &[direction.code().to_owned()], "", reason).await?;
    Ok(())
}

/// Вид причины паузы — текст до двоеточия: «сверка: … не хватает 12» и «… 13» — одна причина.
fn pause_kind(reason: &str) -> &str {
    let kind = reason.split(':').next().unwrap_or(reason).trim();
    if kind.is_empty() {
        "автопауза"
    } else {
        kind
    }
}

/// Добавить причину паузы направлениям `codes` (кошелёк `platform`, '' — без кошелька) и обновить
/// сводку. Сколько из них до этого были открыты.
async fn add_pause(
    conn: &mut PgConnection,
    codes: &[String],
    platform: &str,
    reason: &str,
) -> Result<u64, StorageError> {
    let before = sqlx::query!(
        "SELECT code, auto_paused FROM directions WHERE code = ANY($1) ORDER BY code FOR UPDATE",
        codes
    )
    .fetch_all(&mut *conn)
    .await?;
    // Направление открыто — старые причины (если остались) не в счёт: пауза начинается заново.
    let open: Vec<String> = before
        .iter()
        .filter(|r| !r.auto_paused)
        .map(|r| r.code.clone())
        .collect();
    sqlx::query!(
        "DELETE FROM direction_pauses WHERE direction = ANY($1)",
        &open
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        r#"
        INSERT INTO direction_pauses (direction, platform, kind, reason)
        SELECT unnest($1::text[]), $2, $3, $4
        ON CONFLICT (direction, platform, kind) DO UPDATE SET reason = EXCLUDED.reason
        "#,
        codes,
        platform,
        pause_kind(reason),
        reason
    )
    .execute(&mut *conn)
    .await?;
    sync_pauses(conn, codes).await?;
    Ok(u64::try_from(open.len()).unwrap_or(u64::MAX))
}

/// Причина автопаузы направления (Д-19.9): кошелёк (`None` — без кошелька), текст, с какого
/// времени.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectionPause {
    pub platform: Option<domain::Platform>,
    pub reason: String,
    pub since: chrono::DateTime<chrono::Utc>,
}

/// Все причины автопаузы направления, от ранней к поздней (карточка направления в пульте).
pub async fn pauses(
    conn: &mut PgConnection,
    direction: Direction,
) -> Result<Vec<DirectionPause>, StorageError> {
    let rows = sqlx::query!(
        r#"
        SELECT platform, reason, since FROM direction_pauses
         WHERE direction = $1
         ORDER BY since, kind, platform
        "#,
        direction.code()
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| DirectionPause {
            platform: domain::Platform::from_db_name(&r.platform),
            reason: r.reason,
            since: r.since,
        })
        .collect())
}

/// Сводка в `directions`: на паузе, пока есть причина; показывается самая ранняя.
async fn sync_pauses(conn: &mut PgConnection, codes: &[String]) -> Result<(), StorageError> {
    sqlx::query!(
        r#"
        UPDATE directions d
           SET auto_paused = EXISTS (SELECT 1 FROM direction_pauses p WHERE p.direction = d.code),
               pause_reason = (SELECT p.reason FROM direction_pauses p WHERE p.direction = d.code
                                ORDER BY p.since, p.kind, p.platform LIMIT 1),
               updated_at = now()
         WHERE d.code = ANY($1)
        "#,
        codes
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

/// Автопауза всех направлений, у которых `platform` стоит на стороне `side`: причина добавляется
/// к уже стоящим (Д-19.9). Возвращает, сколько направлений встало на паузу (было открыто).
pub async fn auto_pause_platform(
    conn: &mut PgConnection,
    platform: domain::Platform,
    side: Side,
    reason: &str,
) -> Result<u64, StorageError> {
    let column_is_source = side == Side::Source;
    let codes = sqlx::query_scalar!(
        r#"
        SELECT code FROM directions
         WHERE (CASE WHEN $2 THEN source_platform ELSE target_platform END) = $1
        "#,
        platform.db_name(),
        column_is_source
    )
    .fetch_all(&mut *conn)
    .await?;
    add_pause(conn, &codes, platform.db_name(), reason).await
}

/// Снять с направлений платформы только эту причину паузы этого кошелька (того же вида —
/// [`pause_kind`]); другие причины и паузы из-за другого кошелька остаются (Д-19.9).
/// Сколько направлений открылось.
pub async fn clear_auto_pause_reason(
    conn: &mut PgConnection,
    platform: domain::Platform,
    reason: &str,
) -> Result<u64, StorageError> {
    let codes = sqlx::query_scalar!(
        r#"
        DELETE FROM direction_pauses p USING directions d
         WHERE p.direction = d.code AND d.auto_paused
           AND (d.source_platform = $1 OR d.target_platform = $1)
           AND p.platform IN ($1, '') AND p.kind = $2
        RETURNING p.direction
        "#,
        platform.db_name(),
        pause_kind(reason)
    )
    .fetch_all(&mut *conn)
    .await?;
    if codes.is_empty() {
        return Ok(0);
    }
    sync_pauses(&mut *conn, &codes).await?;
    let opened = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM directions WHERE code = ANY($1) AND NOT auto_paused"#,
        &codes
    )
    .fetch_one(conn)
    .await?;
    Ok(u64::try_from(opened).unwrap_or(0))
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
