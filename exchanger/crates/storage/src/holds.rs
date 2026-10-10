//! Удержания резерва под заявки (`holds`, SPEC §3.4): «не принимаем, если не можем выплатить».
//!
//! Доступно = баланс кошелька по леджеру − активные удержания (`wallet_available`). Проверка
//! и вставка идут под advisory-блокировкой кошелька до конца транзакции: две заявки не займут
//! один и тот же резерв.

use domain::{Asset, Decimal};
use sqlx::PgConnection;

use crate::error::StorageError;

/// Пространство ключей advisory-блокировок резерва.
const LOCK_SPACE: i32 = 7001;

/// Заблокировать резерв кошелька до конца транзакции.
pub async fn lock_wallet(
    conn: &mut PgConnection,
    wallet_account_id: i16,
) -> Result<(), StorageError> {
    sqlx::query!(
        "SELECT pg_advisory_xact_lock($1, $2)",
        LOCK_SPACE,
        i32::from(wallet_account_id)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Доступный резерв кошелька по активу (без блокировки).
pub async fn available(
    conn: &mut PgConnection,
    wallet_account_id: i16,
    asset: Asset,
) -> Result<Decimal, StorageError> {
    let available = sqlx::query_scalar!(
        r#"SELECT available AS "available!" FROM wallet_available WHERE wallet_account_id = $1 AND asset = $2"#,
        wallet_account_id,
        asset.code(),
    )
    .fetch_optional(conn)
    .await?;
    Ok(available.unwrap_or(Decimal::ZERO))
}

/// Удержать `amount` под заявку. Вызывать после [`lock_wallet`] в той же транзакции.
pub async fn place(
    conn: &mut PgConnection,
    order_id: i64,
    wallet_account_id: i16,
    asset: Asset,
    amount: Decimal,
) -> Result<(), StorageError> {
    let free = available(&mut *conn, wallet_account_id, asset).await?;
    if free < amount {
        return Err(StorageError::InsufficientReserve {
            available: free,
            needed: amount,
        });
    }
    sqlx::query!(
        "INSERT INTO holds (order_id, wallet_account_id, asset, amount) VALUES ($1, $2, $3, $4)",
        order_id,
        wallet_account_id,
        asset.code(),
        amount,
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Уменьшить активное удержание до `amount` (точная выплата). Увеличивать нельзя.
pub async fn shrink(
    conn: &mut PgConnection,
    order_id: i64,
    amount: Decimal,
) -> Result<(), StorageError> {
    let updated = sqlx::query!(
        "UPDATE holds SET amount = $2 WHERE order_id = $1 AND state = 'active' AND amount >= $2",
        order_id,
        amount,
    )
    .execute(conn)
    .await?
    .rows_affected();
    if updated == 1 {
        Ok(())
    } else {
        Err(StorageError::HoldState(order_id))
    }
}

/// Закрыть активное удержание: выплата сделана (`consumed`) или не понадобилась (`released`).
/// Нет активного удержания — ничего не делает (оплата счёта без удержания, повторная обработка).
pub async fn close(
    conn: &mut PgConnection,
    order_id: i64,
    consumed: bool,
) -> Result<(), StorageError> {
    let state = if consumed { "consumed" } else { "released" };
    sqlx::query!(
        "UPDATE holds SET state = $2, closed_at = now() WHERE order_id = $1 AND state = 'active'",
        order_id,
        state,
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Активное удержание заявки: (кошелёк, актив, сумма).
pub async fn active(
    conn: &mut PgConnection,
    order_id: i64,
) -> Result<Option<(i16, String, Decimal)>, StorageError> {
    let row = sqlx::query!(
        "SELECT wallet_account_id, asset, amount FROM holds WHERE order_id = $1 AND state = 'active'",
        order_id
    )
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|r| (r.wallet_account_id, r.asset, r.amount)))
}
