//! Заявки через storage API: жизненный цикл с проводками, гонки версий, дубли чеков.

#![allow(clippy::unwrap_used)]

mod common;

use common::{CB, CLIENT, XR, setup};
use domain::fsm::{Effect, Event, OrderState, PostingKind, Settlement};
use domain::pricing::{FeePolicy, quote_check_exchange};
use domain::{Asset, Direction, Step, ledger};
use rust_decimal_macros::dec;
use sqlx::{PgPool, Row};
use storage::StorageError;
use storage::ledger::PostingRefs;
use storage::orders::{Actor, NewOrder, OrderPatch};

fn new_check_order(check: &str) -> NewOrder<'_> {
    NewOrder {
        user_id: CLIENT,
        direction: Direction::CbToXrCheck,
        asset: Asset::Usdt,
        intake_start_param: Some(check),
        invoice_start_param: None,
    }
}

/// П1 целиком (без сети): NEW → INTAKE_PENDING → RECEIVED → PAYOUT_PENDING → COMPLETED,
/// проводки по эффектам, payable закрыт в ноль, комиссия в доходе.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn check_exchange_lifecycle_posts_balanced_ledger(pool: PgPool) {
    setup(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    let order = storage::orders::insert(&mut tx, &new_check_order("CQlife"))
        .await
        .unwrap();
    assert_eq!((order.state, order.version), (OrderState::New, 0));

    let (order, _) = storage::orders::apply_event(
        &mut tx,
        &order,
        Event::LinkAccepted,
        &OrderPatch::default(),
        Actor::System,
        None,
    )
    .await
    .unwrap();

    let quote = quote_check_exchange(
        dec!(100),
        FeePolicy {
            fee_pct: dec!(2.5),
            min_fee: dec!(0.10),
        },
        Step::from_dp(2).unwrap(),
    )
    .unwrap();
    let patch = OrderPatch {
        intake_amount: Some(quote.amount_in),
        fee_pct_applied: Some(dec!(2.5)),
        fee_amount: Some(quote.fee),
        payout_amount: Some(quote.payout),
        ..Default::default()
    };
    let (order, t) = storage::orders::apply_event(
        &mut tx,
        &order,
        Event::CheckActivated { fully_funded: true },
        &patch,
        Actor::System,
        None,
    )
    .await
    .unwrap();
    assert_eq!(t.effects, vec![Effect::Post(PostingKind::Intake)]);
    let intake = ledger::intake(CB, Asset::Usdt, quote.amount_in).unwrap();
    storage::ledger::post(
        &mut tx,
        &intake,
        PostingRefs {
            order_id: Some(order.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let (order, _) = storage::orders::apply_event(
        &mut tx,
        &order,
        Event::Settle(Settlement::Payout),
        &OrderPatch::default(),
        Actor::System,
        None,
    )
    .await
    .unwrap();
    let (order, t) = storage::orders::apply_event(
        &mut tx,
        &order,
        Event::PayoutConfirmed,
        &OrderPatch::default(),
        Actor::System,
        None,
    )
    .await
    .unwrap();
    assert!(t.effects.contains(&Effect::Post(PostingKind::Payout)));
    let payout = ledger::payout(XR, Asset::Usdt, quote.amount_in, quote.payout, quote.fee).unwrap();
    storage::ledger::post(
        &mut tx,
        &payout,
        PostingRefs {
            order_id: Some(order.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert_eq!((order.state, order.version), (OrderState::Completed, 4));

    let sums: Vec<(String, rust_decimal::Decimal)> = sqlx::query_as(
        "SELECT a.code, SUM(e.amount) FROM ledger_entries e JOIN ledger_accounts a ON a.id = e.account_id GROUP BY a.code ORDER BY a.code",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let get = |code: &str| {
        sums.iter()
            .find(|(c, _)| c == code)
            .map(|(_, s)| *s)
            .unwrap()
    };
    assert_eq!(get("liability:payable:USDT"), dec!(0));
    assert_eq!(get("revenue:fee:USDT"), dec!(-2.5));
    assert_eq!(get("asset:xr:USDT"), dec!(902.5));
    assert_eq!(get("asset:cb:USDT"), dec!(1100));

    let row = sqlx::query("SELECT finished_at IS NOT NULL AS done, received_at IS NOT NULL AS recv, (SELECT count(*) FROM order_transitions WHERE order_id = $1) AS n FROM orders WHERE id = $1")
        .bind(order.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(row.get::<bool, _>("done") && row.get::<bool, _>("recv"));
    assert_eq!(
        row.get::<i64, _>("n"),
        5,
        "insert + 4 transitions are journaled"
    );

    let mut conn = pool.acquire().await.unwrap();
    let balances = storage::ledger::wallet_balances(&mut conn).await.unwrap();
    let xr = balances.iter().find(|b| b.label == "xr").unwrap();
    assert_eq!(
        (xr.ledger_balance, xr.held, xr.available),
        (dec!(902.5), dec!(0), dec!(902.5))
    );
}

/// Оптимистичная блокировка: устаревшая версия не перезапишет чужой переход.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn stale_version_is_a_conflict(pool: PgPool) {
    setup(&pool).await;
    let mut conn = pool.acquire().await.unwrap();
    let order = storage::orders::insert(&mut conn, &new_check_order("CQrace"))
        .await
        .unwrap();
    let (_fresh, _) = storage::orders::apply_event(
        &mut conn,
        &order,
        Event::LinkAccepted,
        &OrderPatch::default(),
        Actor::System,
        None,
    )
    .await
    .unwrap();
    let err = storage::orders::apply_event(
        &mut conn,
        &order,
        Event::Rejected,
        &OrderPatch::default(),
        Actor::System,
        None,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, StorageError::Conflict { expected: 0, .. }),
        "{err:?}"
    );
    let current = storage::orders::get(&mut conn, order.id, false)
        .await
        .unwrap();
    assert_eq!(current.state, OrderState::IntakePending);
}

/// Событие, недопустимое в текущем состоянии, отклоняется FSM до UPDATE.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn illegal_event_changes_nothing(pool: PgPool) {
    setup(&pool).await;
    let mut conn = pool.acquire().await.unwrap();
    let order = storage::orders::insert(&mut conn, &new_check_order("CQillegal"))
        .await
        .unwrap();
    let err = storage::orders::apply_event(
        &mut conn,
        &order,
        Event::PayoutConfirmed,
        &OrderPatch::default(),
        Actor::System,
        None,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, StorageError::Fsm(_)), "{err:?}");
    let current = storage::orders::get(&mut conn, order.id, false)
        .await
        .unwrap();
    assert_eq!((current.state, current.version), (OrderState::New, 0));
}

/// Повторная ссылка на тот же чек → «уже в работе» (E16), после завершения — снова можно.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn duplicate_live_check_is_reported(pool: PgPool) {
    setup(&pool).await;
    let mut conn = pool.acquire().await.unwrap();
    let first = storage::orders::insert(&mut conn, &new_check_order("CQdup"))
        .await
        .unwrap();
    let err = storage::orders::insert(&mut conn, &new_check_order("CQdup"))
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::DuplicateLiveCheck), "{err:?}");

    storage::orders::apply_event(
        &mut conn,
        &first,
        Event::Rejected,
        &OrderPatch::default(),
        Actor::System,
        Some("paused"),
    )
    .await
    .unwrap();
    storage::orders::insert(&mut conn, &new_check_order("CQdup"))
        .await
        .unwrap();
}

/// Повторная обработка ответа кошелька не задвоит проводку по той же операции.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn same_operation_cannot_be_posted_twice(pool: PgPool) {
    setup(&pool).await;
    let mut conn = pool.acquire().await.unwrap();
    let order = storage::orders::insert(&mut conn, &new_check_order("CQpost"))
        .await
        .unwrap();
    let op_id: i64 = sqlx::query(
        "INSERT INTO operations (order_id, kind, money_role, via, platform, idempotency_key, start_param, status)
         VALUES ($1, 'activate_check', 'intake', 'userbot', 'cryptobot', 'ord-x-intake-1', 'CQpost', 'succeeded') RETURNING id",
    )
    .bind(order.id)
    .fetch_one(&mut *conn)
    .await
    .unwrap()
    .get(0);
    let intake = ledger::intake(CB, Asset::Usdt, dec!(10)).unwrap();
    let refs = PostingRefs {
        order_id: Some(order.id),
        operation_id: Some(op_id),
        ..Default::default()
    };
    storage::ledger::post(&mut conn, &intake, refs)
        .await
        .unwrap();
    let err = storage::ledger::post(&mut conn, &intake, refs)
        .await
        .unwrap_err();
    assert!(
        matches!(err, StorageError::AlreadyPosted(id) if id == op_id),
        "{err:?}"
    );
}

/// Проводка на несуществующий счёт не создаёт ничего.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn posting_to_unknown_account_fails(pool: PgPool) {
    setup(&pool).await;
    let mut conn = pool.acquire().await.unwrap();
    let ton = ledger::intake(XR, Asset::Ton, dec!(1)).unwrap();
    let err = storage::ledger::post(&mut conn, &ton, PostingRefs::default())
        .await
        .unwrap_err();
    assert!(
        matches!(err, StorageError::UnknownAccount(ref c) if c == "asset:xr:TON"),
        "{err:?}"
    );
}
