//! Outbox операций и удержания резерва на живом PostgreSQL.

#![allow(clippy::unwrap_used)]

mod common;

use chrono::Utc;
use common::{CB, CLIENT, XR, setup};
use domain::fsm::{MoneyRole, OpKind};
use domain::{Asset, Direction};
use rust_decimal_macros::dec;
use serde_json::json;
use sqlx::PgPool;
use storage::StorageError;
use storage::operations::{NewOperation, OpStatus};
use storage::orders::NewOrder;

async fn order(pool: &PgPool, check: &str) -> i64 {
    let mut conn = pool.acquire().await.unwrap();
    storage::orders::insert(
        &mut conn,
        &NewOrder {
            user_id: CLIENT,
            direction: Direction::CbToXrCheck,
            asset: Asset::Usdt,
            intake_start_param: Some(check),
            invoice_start_param: None,
        },
    )
    .await
    .unwrap()
    .id
}

async fn wallet(pool: &PgPool, platform: domain::Platform) -> i16 {
    let mut conn = pool.acquire().await.unwrap();
    storage::wallets::active(&mut conn, platform)
        .await
        .unwrap()
        .unwrap()
        .0
}

fn op<'a>(order_id: i64, kind: OpKind, key: &'a str, wallet: i16) -> NewOperation<'a> {
    let (role, platform, start_param, amount) = match kind {
        OpKind::ActivateCheck => (MoneyRole::Intake, CB, Some("CQAbCdEfGhIj"), None),
        _ => (MoneyRole::Settle, XR, None, Some(dec!(97.5))),
    };
    NewOperation {
        order_id,
        kind,
        role,
        platform,
        wallet_account_id: wallet,
        idempotency_key: key,
        start_param,
        amount,
        asset: Asset::Usdt,
        random_id: 0x5EED_0000_0000_0001,
    }
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn operation_lifecycle_claim_finish_release_retry(pool: PgPool) {
    setup(&pool).await;
    let id = order(&pool, "CQAbCdEfGhIj").await;
    let (cb, xr) = (wallet(&pool, CB).await, wallet(&pool, XR).await);
    let mut conn = pool.acquire().await.unwrap();
    let activate = storage::operations::insert(
        &mut conn,
        &op(id, OpKind::ActivateCheck, "ord-1-intake-1", cb),
    )
    .await
    .unwrap();

    let claimed = storage::operations::claim_due(&mut conn, 10).await.unwrap();
    assert_eq!(claimed.len(), 1);
    let row = &claimed[0];
    assert_eq!(row.id, activate);
    assert_eq!(row.status, OpStatus::Dispatched);
    assert_eq!((row.attempts, row.max_attempts), (1, 8));
    assert_eq!(row.random_id, 0x5EED_0000_0000_0001);
    assert!(
        storage::operations::claim_due(&mut conn, 10)
            .await
            .unwrap()
            .is_empty(),
        "уже в работе"
    );

    // Не выполнялось — попытка не считается.
    storage::operations::release(&mut conn, activate, Utc::now(), "flood wait")
        .await
        .unwrap();
    let again = storage::operations::claim_due(&mut conn, 10).await.unwrap();
    assert_eq!(again[0].attempts, 1);
    // Неизвестный исход активации — повтор с подсчётом попытки.
    assert!(
        storage::operations::retry(&mut conn, activate, Utc::now(), "no reply")
            .await
            .unwrap()
    );
    let third = storage::operations::claim_due(&mut conn, 10).await.unwrap();
    assert_eq!(third[0].attempts, 2);
    storage::operations::finish(
        &mut conn,
        activate,
        OpStatus::Succeeded,
        &json!({"received": "100"}),
        None,
        None,
    )
    .await
    .unwrap();
    // Итог записан — второй раз нельзя.
    assert!(matches!(
        storage::operations::finish(
            &mut conn,
            activate,
            OpStatus::Failed,
            &json!({}),
            None,
            None
        )
        .await,
        Err(StorageError::OperationState(_))
    ));

    // Выплата: одна попытка, повтор с подсчётом запрещён.
    let payout = storage::operations::insert(
        &mut conn,
        &op(id, OpKind::CreatePayoutCheck, "ord-1-payout-1", xr),
    )
    .await
    .unwrap();
    let claimed = storage::operations::claim_due(&mut conn, 10).await.unwrap();
    assert_eq!((claimed[0].id, claimed[0].max_attempts), (payout, 1));
    assert!(
        !storage::operations::retry(&mut conn, payout, Utc::now(), "x")
            .await
            .unwrap()
    );
    storage::operations::finish(
        &mut conn,
        payout,
        OpStatus::Unknown,
        &json!({}),
        None,
        Some("timeout"),
    )
    .await
    .unwrap();
    let in_flight = storage::operations::in_flight(&mut conn).await.unwrap();
    assert_eq!(in_flight.len(), 1);
    assert_eq!(in_flight[0].status, OpStatus::Unknown);
    storage::operations::finish(
        &mut conn,
        payout,
        OpStatus::Succeeded,
        &json!({}),
        Some("t_AbCdEfGhIjKlMnO"),
        None,
    )
    .await
    .unwrap();
    let ops = storage::operations::for_order(&mut conn, id).await.unwrap();
    assert_eq!(ops[1].external_id.as_deref(), Some("t_AbCdEfGhIjKlMnO"));
    assert_eq!(
        storage::operations::count_for_order(&mut conn, id, OpKind::CreatePayoutCheck)
            .await
            .unwrap(),
        1
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn one_live_settle_per_order_and_one_activation_per_check(pool: PgPool) {
    setup(&pool).await;
    let id = order(&pool, "CQAbCdEfGhIj").await;
    let (cb, xr) = (wallet(&pool, CB).await, wallet(&pool, XR).await);
    let mut conn = pool.acquire().await.unwrap();
    storage::operations::insert(
        &mut conn,
        &op(id, OpKind::CreatePayoutCheck, "ord-1-payout-1", xr),
    )
    .await
    .unwrap();
    let second = storage::operations::insert(
        &mut conn,
        &op(id, OpKind::CreatePayoutCheck, "ord-1-payout-2", xr),
    )
    .await;
    assert!(second.is_err(), "вторая живая выплата по заявке");

    storage::operations::insert(
        &mut conn,
        &op(id, OpKind::ActivateCheck, "ord-1-intake-1", cb),
    )
    .await
    .unwrap();
    let other = order(&pool, "CQzzzzzzzzzz").await;
    let same_check = storage::operations::insert(
        &mut conn,
        &op(other, OpKind::ActivateCheck, "ord-2-intake-1", cb),
    )
    .await;
    assert!(same_check.is_err(), "один чек — одна активация");
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn holds_never_exceed_the_reserve(pool: PgPool) {
    setup(&pool).await;
    let xr = wallet(&pool, XR).await;
    let a = order(&pool, "CQAbCdEfGhIj").await;
    let b = order(&pool, "CQzzzzzzzzzz").await;
    let mut tx = pool.begin().await.unwrap();
    storage::holds::lock_wallet(&mut tx, xr).await.unwrap();
    storage::holds::place(&mut tx, a, xr, Asset::Usdt, dec!(700))
        .await
        .unwrap();
    assert_eq!(
        storage::holds::available(&mut tx, xr, Asset::Usdt)
            .await
            .unwrap(),
        dec!(300)
    );
    let too_much = storage::holds::place(&mut tx, b, xr, Asset::Usdt, dec!(300.01)).await;
    assert!(matches!(
        too_much,
        Err(StorageError::InsufficientReserve { .. })
    ));
    tx.commit().await.unwrap();

    let mut conn = pool.acquire().await.unwrap();
    storage::holds::shrink(&mut conn, a, dec!(97.5))
        .await
        .unwrap();
    assert!(matches!(
        storage::holds::shrink(&mut conn, a, dec!(98)).await,
        Err(StorageError::HoldState(_))
    ));
    assert_eq!(
        storage::holds::available(&mut conn, xr, Asset::Usdt)
            .await
            .unwrap(),
        dec!(902.5)
    );
    storage::holds::close(&mut conn, a, false).await.unwrap();
    assert_eq!(storage::holds::active(&mut conn, a).await.unwrap(), None);
    assert_eq!(
        storage::holds::available(&mut conn, xr, Asset::Usdt)
            .await
            .unwrap(),
        dec!(1000)
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn directions_and_steps_are_read(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let d = storage::directions::get(&mut conn, Direction::CbToXrCheck)
        .await
        .unwrap();
    assert_eq!((d.fee_pct, d.min_fee), (dec!(2.5), dec!(0.10)));
    assert!(!d.accepting(), "в сиде направления закрыты");
    storage::directions::set_open(&mut conn, Direction::CbToXrCheck, true)
        .await
        .unwrap();
    storage::directions::auto_pause(&mut conn, Direction::CbToXrCheck, "reserve low")
        .await
        .unwrap();
    let d = storage::directions::get(&mut conn, Direction::CbToXrCheck)
        .await
        .unwrap();
    assert!(d.is_open && d.auto_paused && !d.accepting());
    assert_eq!(d.pause_reason.as_deref(), Some("reserve low"));
    assert_eq!(
        storage::directions::payout_step(&mut conn, XR, Asset::Usdt)
            .await
            .unwrap(),
        dec!(0.01)
    );
}
