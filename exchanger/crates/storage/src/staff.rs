//! Персонал пульта (`staff`, SPEC §7.2): владелец, админы, операторы.
//!
//! Владелец ровно один (`staff_single_owner`). Отключённый сотрудник (`disabled_at`) для
//! пульта — посторонний.

use chrono::{DateTime, Utc};
use domain::Decimal;
use sqlx::PgConnection;

use crate::error::StorageError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Operator,
    Admin,
    Owner,
}

impl Role {
    pub const ALL: [Role; 3] = [Role::Owner, Role::Admin, Role::Operator];

    pub const fn as_db_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Admin => "admin",
            Role::Operator => "operator",
        }
    }

    pub fn from_db_str(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_db_str() == s)
    }

    /// Потолок ручной выплаты по умолчанию (SPEC П9): оператор 50, админ 300, владелец — без.
    pub fn default_manual_payout(self) -> Option<Decimal> {
        match self {
            Role::Operator => Some(Decimal::from(50)),
            Role::Admin => Some(Decimal::from(300)),
            Role::Owner => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaffMember {
    pub tg_user_id: i64,
    pub role: Role,
    pub display_name: String,
    /// Личный потолок ручной выплаты; `None` — по роли.
    pub max_manual_payout: Option<Decimal>,
    pub added_by: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
}

impl StaffMember {
    pub fn active(&self) -> bool {
        self.disabled_at.is_none()
    }

    /// Сколько этот сотрудник может выплатить вручную; `None` — без ограничения.
    pub fn manual_payout_limit(&self) -> Option<Decimal> {
        match self.role {
            Role::Owner => None,
            role => self.max_manual_payout.or(role.default_manual_payout()),
        }
    }
}

struct Raw {
    tg_user_id: i64,
    role: String,
    display_name: String,
    max_manual_payout: Option<Decimal>,
    added_by: Option<i64>,
    created_at: DateTime<Utc>,
    disabled_at: Option<DateTime<Utc>>,
}

impl TryFrom<Raw> for StaffMember {
    type Error = StorageError;

    fn try_from(r: Raw) -> Result<Self, StorageError> {
        Ok(Self {
            tg_user_id: r.tg_user_id,
            role: Role::from_db_str(&r.role)
                .ok_or_else(|| StorageError::corrupt("staff.role", &r.role))?,
            display_name: r.display_name,
            max_manual_payout: r.max_manual_payout,
            added_by: r.added_by,
            created_at: r.created_at,
            disabled_at: r.disabled_at,
        })
    }
}

/// Сотрудник по id Telegram (и отключённый тоже).
pub async fn get(
    conn: &mut PgConnection,
    tg_user_id: i64,
) -> Result<Option<StaffMember>, StorageError> {
    let row = sqlx::query_as!(
        Raw,
        r#"
        SELECT tg_user_id, role, display_name, max_manual_payout, added_by, created_at, disabled_at
          FROM staff WHERE tg_user_id = $1
        "#,
        tg_user_id
    )
    .fetch_optional(conn)
    .await?;
    row.map(TryInto::try_into).transpose()
}

/// Действующий сотрудник; отключённый — `None`.
pub async fn active(
    conn: &mut PgConnection,
    tg_user_id: i64,
) -> Result<Option<StaffMember>, StorageError> {
    Ok(get(conn, tg_user_id).await?.filter(StaffMember::active))
}

/// Весь персонал: сначала действующие по старшинству роли.
pub async fn list(conn: &mut PgConnection) -> Result<Vec<StaffMember>, StorageError> {
    let rows = sqlx::query_as!(
        Raw,
        r#"
        SELECT tg_user_id, role, display_name, max_manual_payout, added_by, created_at, disabled_at
          FROM staff
         ORDER BY disabled_at IS NOT NULL,
                  CASE role WHEN 'owner' THEN 0 WHEN 'admin' THEN 1 ELSE 2 END,
                  created_at
        "#
    )
    .fetch_all(conn)
    .await?;
    rows.into_iter().map(TryInto::try_into).collect()
}

/// Действующий владелец.
pub async fn owner(conn: &mut PgConnection) -> Result<Option<StaffMember>, StorageError> {
    let row = sqlx::query_as!(
        Raw,
        r#"
        SELECT tg_user_id, role, display_name, max_manual_payout, added_by, created_at, disabled_at
          FROM staff WHERE role = 'owner' AND disabled_at IS NULL
        "#
    )
    .fetch_optional(conn)
    .await?;
    row.map(TryInto::try_into).transpose()
}

/// Владелец из окружения сервера (`OWNER_TG_ID`): создать, если владельца ещё нет. Если
/// владелец уже другой — ничего не меняем и возвращаем его (смена владельца — только руками
/// через пульт, с подтверждением).
pub async fn ensure_owner(
    conn: &mut PgConnection,
    tg_user_id: i64,
    display_name: &str,
) -> Result<StaffMember, StorageError> {
    if let Some(owner) = owner(&mut *conn).await? {
        return Ok(owner);
    }
    sqlx::query!(
        r#"
        INSERT INTO staff (tg_user_id, role, display_name) VALUES ($1, 'owner', $2)
        ON CONFLICT (tg_user_id) DO UPDATE SET role = 'owner', disabled_at = NULL
        "#,
        tg_user_id,
        display_name
    )
    .execute(&mut *conn)
    .await?;
    get(conn, tg_user_id)
        .await?
        .ok_or_else(|| StorageError::corrupt("staff", "owner row missing after insert"))
}

/// Добавить сотрудника или вернуть отключённого (роль и имя — новые).
pub async fn add(
    conn: &mut PgConnection,
    tg_user_id: i64,
    role: Role,
    display_name: &str,
    added_by: i64,
) -> Result<(), StorageError> {
    sqlx::query!(
        r#"
        INSERT INTO staff (tg_user_id, role, display_name, added_by) VALUES ($1, $2, $3, $4)
        ON CONFLICT (tg_user_id) DO UPDATE
           SET role = EXCLUDED.role, display_name = EXCLUDED.display_name,
               added_by = EXCLUDED.added_by, disabled_at = NULL
        "#,
        tg_user_id,
        role.as_db_str(),
        display_name,
        added_by
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn set_role(
    conn: &mut PgConnection,
    tg_user_id: i64,
    role: Role,
) -> Result<(), StorageError> {
    sqlx::query!(
        "UPDATE staff SET role = $2 WHERE tg_user_id = $1",
        tg_user_id,
        role.as_db_str()
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn set_manual_payout(
    conn: &mut PgConnection,
    tg_user_id: i64,
    limit: Option<Decimal>,
) -> Result<(), StorageError> {
    sqlx::query!(
        "UPDATE staff SET max_manual_payout = $2 WHERE tg_user_id = $1",
        tg_user_id,
        limit
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn disable(conn: &mut PgConnection, tg_user_id: i64) -> Result<(), StorageError> {
    sqlx::query!(
        "UPDATE staff SET disabled_at = now() WHERE tg_user_id = $1 AND disabled_at IS NULL",
        tg_user_id
    )
    .execute(conn)
    .await?;
    Ok(())
}
