//! Запись проводок в леджер. Баланс по активу проверяет `domain::ledger::Posting`
//! и повторно — отложенный триггер `ledger_entries_balanced` при коммите.

use domain::ledger::{AccountCode, Posting};
use domain::{Asset, Decimal};
use sqlx::PgConnection;

use crate::error::{StorageError, unique_violation};

/// К чему относится проводка.
#[derive(Debug, Clone, Copy, Default)]
pub struct PostingRefs<'a> {
    pub order_id: Option<i64>,
    pub operation_id: Option<i64>,
    pub rebalance_id: Option<i64>,
    /// Обязательна для ручных видов (`adjustment`, `loss`, `capital_in`, `manual_close`, …).
    pub memo: Option<&'a str>,
    /// `system` или `staff:<id>`.
    pub created_by: Option<&'a str>,
}

/// Создать счёт, если его нет. Тип берётся из префикса кода (`asset:`, `liability:` …).
pub async fn ensure_account(
    conn: &mut PgConnection,
    code: &AccountCode,
    asset: Asset,
    wallet_account_id: Option<i16>,
) -> Result<i32, StorageError> {
    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO ledger_accounts (code, type, asset, wallet_account_id)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (code) DO UPDATE SET code = EXCLUDED.code
        RETURNING id
        "#,
        code.as_str(),
        code.account_type(),
        asset.code(),
        wallet_account_id,
    )
    .fetch_one(conn)
    .await?;
    Ok(id)
}

/// Записать проводку; возвращает `ledger_transactions.id`.
///
/// Вызывать внутри транзакции вместе с переходом заявки: шапка и строки проводки —
/// два INSERT, и без общей транзакции сбой между ними оставил бы пустую шапку.
pub async fn post(
    conn: &mut PgConnection,
    posting: &Posting,
    refs: PostingRefs<'_>,
) -> Result<i64, StorageError> {
    let codes: Vec<String> = posting
        .lines()
        .iter()
        .map(|l| l.account.as_str().to_owned())
        .collect();
    let accounts = sqlx::query!(
        r#"SELECT id, code FROM ledger_accounts WHERE code = ANY($1)"#,
        &codes,
    )
    .fetch_all(&mut *conn)
    .await?;

    let mut account_ids = Vec::with_capacity(codes.len());
    for code in &codes {
        let id = accounts
            .iter()
            .find(|a| &a.code == code)
            .map(|a| a.id)
            .ok_or_else(|| StorageError::UnknownAccount(code.clone()))?;
        account_ids.push(id);
    }
    let assets: Vec<String> = posting
        .lines()
        .iter()
        .map(|l| l.asset.code().to_owned())
        .collect();
    let amounts: Vec<Decimal> = posting.lines().iter().map(|l| l.amount).collect();

    let tx_id = sqlx::query_scalar!(
        r#"
        INSERT INTO ledger_transactions (kind, order_id, operation_id, rebalance_id, memo, created_by)
        VALUES ($1, $2, $3, $4, $5, COALESCE($6, 'system'))
        RETURNING id
        "#,
        posting.kind().as_db_str(),
        refs.order_id,
        refs.operation_id,
        refs.rebalance_id,
        refs.memo,
        refs.created_by,
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| match (unique_violation(&e), refs.operation_id) {
        (Some("ledger_tx_once_per_operation"), Some(op)) => StorageError::AlreadyPosted(op),
        _ => StorageError::Db(e),
    })?;

    sqlx::query!(
        r#"
        INSERT INTO ledger_entries (tx_id, account_id, asset, amount)
        SELECT $1, account_id, asset, amount
          FROM UNNEST($2::int4[], $3::text[], $4::numeric[]) AS t(account_id, asset, amount)
        "#,
        tx_id,
        &account_ids,
        &assets,
        &amounts,
    )
    .execute(&mut *conn)
    .await?;

    Ok(tx_id)
}

/// Строка представления `wallet_available` (SPEC §3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletBalance {
    pub wallet_account_id: i16,
    pub label: String,
    pub asset: String,
    pub ledger_balance: Decimal,
    pub held: Decimal,
    pub available: Decimal,
}

pub async fn wallet_balances(conn: &mut PgConnection) -> Result<Vec<WalletBalance>, StorageError> {
    let rows = sqlx::query_as!(
        WalletBalance,
        r#"
        SELECT wallet_account_id AS "wallet_account_id!",
               label             AS "label!",
               asset             AS "asset!",
               ledger_balance    AS "ledger_balance!",
               held              AS "held!",
               available         AS "available!"
          FROM wallet_available
         ORDER BY wallet_account_id, asset
        "#
    )
    .fetch_all(conn)
    .await?;
    Ok(rows)
}
