//! Личные условия клиента по направлению (`user_terms`): комиссия, минимальная комиссия, лимиты
//! заявки, освобождение от лимита нового клиента. Пустое поле — как у направления.
//!
//! Личная комиссия заменяет базу (фикс или ступень), рычаг резерва и выгодный курс действуют
//! поверх неё — это считает `domain::fees::decide`, здесь только лимиты и минимальная комиссия.

use chrono::{DateTime, Utc};
use domain::{Decimal, Direction};
use sqlx::PgConnection;

use crate::directions::DirectionRow;
use crate::error::StorageError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserTerms {
    pub direction: Direction,
    pub fee_pct: Option<Decimal>,
    pub min_fee: Option<Decimal>,
    pub min_amount: Option<Decimal>,
    pub max_amount: Option<Decimal>,
    pub newcomer_exempt: bool,
    pub note: Option<String>,
    pub updated_by: Option<i64>,
    pub updated_at: DateTime<Utc>,
}

/// Что меняем. `None` — оставить как есть, `Some(None)` — вернуть «как у направления».
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserTermsChange {
    pub fee_pct: Option<Option<Decimal>>,
    pub min_fee: Option<Option<Decimal>>,
    pub min_amount: Option<Option<Decimal>>,
    pub max_amount: Option<Option<Decimal>>,
    pub newcomer_exempt: Option<bool>,
    pub note: Option<Option<String>>,
}

impl UserTerms {
    /// Пустые условия направления (ничего личного).
    pub fn empty(direction: Direction) -> Self {
        Self {
            direction,
            fee_pct: None,
            min_fee: None,
            min_amount: None,
            max_amount: None,
            newcomer_exempt: false,
            note: None,
            updated_by: None,
            updated_at: DateTime::<Utc>::UNIX_EPOCH,
        }
    }

    /// Есть ли что-то личное.
    pub fn is_personal(&self) -> bool {
        self.fee_pct.is_some()
            || self.min_fee.is_some()
            || self.min_amount.is_some()
            || self.max_amount.is_some()
            || self.newcomer_exempt
    }

    /// Лимиты и минимальная комиссия направления для этого клиента. Ставку не трогает:
    /// личная ставка идёт в `domain::fees::FeeInputs::personal_pct`.
    pub fn apply(&self, dir: &DirectionRow) -> DirectionRow {
        let mut out = dir.clone();
        if let Some(min_fee) = self.min_fee {
            out.min_fee = min_fee;
        }
        if let Some(min) = self.min_amount {
            out.min_amount = min;
        }
        if let Some(max) = self.max_amount {
            out.max_amount = max;
        }
        out.new_user_max_amount = if self.newcomer_exempt {
            out.max_amount
        } else {
            out.new_user_max_amount.min(out.max_amount)
        };
        out
    }

    /// Новое состояние после изменения.
    pub fn changed(&self, change: &UserTermsChange) -> UserTerms {
        let mut next = self.clone();
        if let Some(v) = change.fee_pct {
            next.fee_pct = v;
        }
        if let Some(v) = change.min_fee {
            next.min_fee = v;
        }
        if let Some(v) = change.min_amount {
            next.min_amount = v;
        }
        if let Some(v) = change.max_amount {
            next.max_amount = v;
        }
        if let Some(v) = change.newcomer_exempt {
            next.newcomer_exempt = v;
        }
        if let Some(v) = &change.note {
            next.note = v.clone();
        }
        next
    }
}

/// Условия направления для клиента: строка направления с его личными условиями.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effective {
    pub dir: DirectionRow,
    pub personal: UserTerms,
    /// Действует ли к нему лимит нового клиента (включён глобально и не снят лично). Если
    /// нет — `dir.new_user_max_amount` уже равен `dir.max_amount`, а суточный лимит — обычный.
    pub newcomer_limit: bool,
}

struct Raw {
    direction: String,
    fee_pct: Option<Decimal>,
    min_fee: Option<Decimal>,
    min_amount: Option<Decimal>,
    max_amount: Option<Decimal>,
    newcomer_exempt: bool,
    note: Option<String>,
    updated_by: Option<i64>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<Raw> for UserTerms {
    type Error = StorageError;

    fn try_from(r: Raw) -> Result<Self, StorageError> {
        Ok(Self {
            direction: r
                .direction
                .parse()
                .map_err(|_| StorageError::corrupt("user_terms.direction", &r.direction))?,
            fee_pct: r.fee_pct,
            min_fee: r.min_fee,
            min_amount: r.min_amount,
            max_amount: r.max_amount,
            newcomer_exempt: r.newcomer_exempt,
            note: r.note,
            updated_by: r.updated_by,
            updated_at: r.updated_at,
        })
    }
}

/// Личные условия клиента по направлению; нет строки — пустые.
pub async fn get(
    conn: &mut PgConnection,
    user_id: i64,
    direction: Direction,
) -> Result<UserTerms, StorageError> {
    let row = sqlx::query_as!(
        Raw,
        r#"
        SELECT direction, fee_pct, min_fee, min_amount, max_amount, newcomer_exempt, note,
               updated_by, updated_at
          FROM user_terms WHERE user_id = $1 AND direction = $2
        "#,
        user_id,
        direction.code()
    )
    .fetch_optional(conn)
    .await?;
    match row {
        Some(r) => r.try_into(),
        None => Ok(UserTerms::empty(direction)),
    }
}

/// Все личные условия клиента (только направления, где что-то задано).
pub async fn for_user(
    conn: &mut PgConnection,
    user_id: i64,
) -> Result<Vec<UserTerms>, StorageError> {
    let rows = sqlx::query_as!(
        Raw,
        r#"
        SELECT direction, fee_pct, min_fee, min_amount, max_amount, newcomer_exempt, note,
               updated_by, updated_at
          FROM user_terms WHERE user_id = $1 ORDER BY direction
        "#,
        user_id
    )
    .fetch_all(conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let terms: UserTerms = r.try_into()?;
        if terms.is_personal() || terms.note.is_some() {
            out.push(terms);
        }
    }
    Ok(out)
}

/// Есть ли у клиента хоть одно личное условие (клиентский бот: общий кэш условий или свой).
pub async fn has_any(conn: &mut PgConnection, user_id: i64) -> Result<bool, StorageError> {
    Ok(sqlx::query_scalar!(
        r#"
        SELECT EXISTS (
            SELECT 1 FROM user_terms
             WHERE user_id = $1
               AND (fee_pct IS NOT NULL OR min_fee IS NOT NULL OR min_amount IS NOT NULL
                    OR max_amount IS NOT NULL OR newcomer_exempt)
        ) AS "exists!"
        "#,
        user_id
    )
    .fetch_one(conn)
    .await?)
}

/// Условия направления для клиента.
pub async fn effective(
    conn: &mut PgConnection,
    direction: Direction,
    user_id: i64,
) -> Result<Effective, StorageError> {
    let dir = crate::directions::get(&mut *conn, direction).await?;
    let personal = get(&mut *conn, user_id, direction).await?;
    let global = crate::settings::flag(&mut *conn, NEWCOMER_LIMIT, true).await?;
    let mut dir = personal.apply(&dir);
    let newcomer_limit = global && !personal.newcomer_exempt;
    if !newcomer_limit {
        dir.new_user_max_amount = dir.max_amount;
    }
    Ok(Effective {
        dir,
        personal,
        newcomer_limit,
    })
}

/// Настройка «лимит нового клиента» (`{"on": true}`), глобально.
pub const NEWCOMER_LIMIT: &str = "newcomer_limit";

/// Записать личные условия целиком (строка создаётся при первом изменении).
pub async fn save(
    conn: &mut PgConnection,
    user_id: i64,
    terms: &UserTerms,
    staff_id: i64,
) -> Result<(), StorageError> {
    sqlx::query!(
        r#"
        INSERT INTO user_terms (user_id, direction, fee_pct, min_fee, min_amount, max_amount,
                                newcomer_exempt, note, updated_by, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now())
        ON CONFLICT (user_id, direction) DO UPDATE
           SET fee_pct = EXCLUDED.fee_pct, min_fee = EXCLUDED.min_fee,
               min_amount = EXCLUDED.min_amount, max_amount = EXCLUDED.max_amount,
               newcomer_exempt = EXCLUDED.newcomer_exempt, note = EXCLUDED.note,
               updated_by = EXCLUDED.updated_by, updated_at = now()
        "#,
        user_id,
        terms.direction.code(),
        terms.fee_pct,
        terms.min_fee,
        terms.min_amount,
        terms.max_amount,
        terms.newcomer_exempt,
        terms.note,
        staff_id,
    )
    .execute(conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    fn dir() -> DirectionRow {
        DirectionRow {
            direction: Direction::CbToXrCheck,
            fee_pct: dec!(2.5),
            min_fee: dec!(0.10),
            skew_k_pct: dec!(0.75),
            fee_floor_pct: dec!(1.5),
            fee_cap_pct: dec!(3.5),
            min_amount: dec!(2),
            max_amount: dec!(300),
            new_user_max_amount: dec!(100),
            fiat_buffer_pct: dec!(1),
            is_open: true,
            auto_paused: false,
            pause_reason: None,
            fee_mode: domain::fees::FeeMode::Flat,
            fee_tiers: None,
        }
    }

    #[test]
    fn empty_terms_change_nothing() {
        let d = dir();
        assert_eq!(UserTerms::empty(d.direction).apply(&d), d);
        assert!(!UserTerms::empty(d.direction).is_personal());
    }

    #[test]
    fn personal_fee_is_not_applied_to_the_row() {
        let d = dir();
        let mut t = UserTerms::empty(d.direction);
        t.fee_pct = Some(dec!(1));
        t.min_fee = Some(dec!(0.02));
        let e = t.apply(&d);
        assert_eq!(
            (e.fee_pct, e.fee_floor_pct, e.fee_cap_pct),
            (d.fee_pct, d.fee_floor_pct, d.fee_cap_pct)
        );
        assert_eq!(e.min_fee, dec!(0.02));
        assert!(t.is_personal());
    }

    #[test]
    fn limits_and_newcomer_exemption() {
        let d = dir();
        let mut t = UserTerms::empty(d.direction);
        t.max_amount = Some(dec!(50));
        let e = t.apply(&d);
        assert_eq!((e.max_amount, e.new_user_max_amount), (dec!(50), dec!(50)));
        t.max_amount = Some(dec!(1000));
        t.newcomer_exempt = true;
        let e = t.apply(&d);
        assert_eq!(
            (e.max_amount, e.new_user_max_amount),
            (dec!(1000), dec!(1000))
        );
        t.newcomer_exempt = false;
        let e = t.apply(&d);
        assert_eq!(e.new_user_max_amount, dec!(100));
    }
}
