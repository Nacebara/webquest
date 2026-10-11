//! Запросы клиентского бота (`storage::client`) на живом PostgreSQL: сообщение прогресса,
//! вид заявки, история, бан, живая заявка по чеку, выборка для пересинхронизации.

#![allow(clippy::unwrap_used)]

mod common;

use chrono::{Duration, Utc};
use common::{CB, CLIENT, setup};
use domain::fsm::{Event, OrderState};
use domain::{Asset, Direction};
use rust_decimal_macros::dec;
use sqlx::PgPool;
use storage::client::{self, ProgressMessage};
use storage::orders::{Actor, NewOrder, OrderPatch, OrderRow};

async fn new_order(pool: &PgPool, user: i64, check: &str) -> OrderRow {
    let mut tx = pool.begin().await.unwrap();
    let order = storage::orders::insert(
        &mut tx,
        &NewOrder {
            user_id: user,
            direction: Direction::CbToXrCheck,
            asset: Asset::Usdt,
            intake_start_param: Some(check),
            invoice_start_param: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    order
}

async fn apply(pool: &PgPool, order: &OrderRow, event: Event, patch: OrderPatch<'_>) -> OrderRow {
    let mut tx = pool.begin().await.unwrap();
    let order = storage::orders::get(&mut tx, order.id, true).await.unwrap();
    let (order, _) =
        storage::orders::apply_event(&mut tx, &order, event, &patch, Actor::System, None)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    order
}

async fn add_user(pool: &PgPool, id: i64) {
    let mut conn = pool.acquire().await.unwrap();
    storage::users::upsert(&mut conn, id, &storage::users::Profile::default())
        .await
        .unwrap();
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn progress_message_round_trip(pool: PgPool) {
    setup(&pool).await;
    let order = new_order(&pool, CLIENT, "CQprogress01").await;
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        client::progress_message(&mut conn, order.id).await.unwrap(),
        None
    );
    assert!(
        !client::is_progress_message(&mut conn, CLIENT, 77)
            .await
            .unwrap()
    );

    let msg = ProgressMessage {
        chat_id: CLIENT,
        message_id: 77,
    };
    client::set_progress_message(&mut conn, order.id, msg)
        .await
        .unwrap();
    assert_eq!(
        client::progress_message(&mut conn, order.id).await.unwrap(),
        Some(msg)
    );
    assert!(
        client::is_progress_message(&mut conn, CLIENT, 77)
            .await
            .unwrap()
    );
    assert!(
        !client::is_progress_message(&mut conn, CLIENT, 78)
            .await
            .unwrap()
    );
    assert!(matches!(
        client::set_progress_message(&mut conn, 999_999, msg).await,
        Err(storage::StorageError::NotFound(999_999))
    ));

    // Запись сообщения не мешает движку: следующий переход по свежей заявке проходит.
    drop(conn);
    let order = apply(&pool, &order, Event::LinkAccepted, OrderPatch::default()).await;
    assert_eq!(order.state, OrderState::IntakePending);
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn order_view_reports_amounts_state_time_and_owner(pool: PgPool) {
    setup(&pool).await;
    add_user(&pool, 222).await;
    let order = new_order(&pool, CLIENT, "CQviewview01").await;
    let order = apply(&pool, &order, Event::LinkAccepted, OrderPatch::default()).await;
    let order = apply(
        &pool,
        &order,
        Event::CheckRejected,
        OrderPatch {
            failure_code: Some("check_not_found"),
            ..OrderPatch::default()
        },
    )
    .await;

    let mut conn = pool.acquire().await.unwrap();
    let view = client::order_view(&mut conn, order.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(view.state, OrderState::IntakeFailed);
    assert_eq!(view.public_id, order.public_id);
    assert_eq!(view.user_id, CLIENT);
    assert_eq!(view.direction, Direction::CbToXrCheck);
    assert_eq!(view.failure_code.as_deref(), Some("check_not_found"));
    assert!(view.finished_at.is_some());
    assert!(view.state_since >= view.created_at);
    assert_eq!(view.progress, None);

    let own = client::order_view_for_user(&mut conn, CLIENT, &order.public_id)
        .await
        .unwrap();
    assert_eq!(own.map(|v| v.id), Some(order.id));
    let foreign = client::order_view_for_user(&mut conn, 222, &order.public_id)
        .await
        .unwrap();
    assert_eq!(foreign, None, "чужая заявка не отдаётся");
    assert_eq!(client::order_view(&mut conn, 999_999).await.unwrap(), None);
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn history_is_newest_first_and_paged(pool: PgPool) {
    setup(&pool).await;
    add_user(&pool, 222).await;
    let mut ids = Vec::new();
    for i in 0..3 {
        ids.push(new_order(&pool, CLIENT, &format!("CQhistory{i:03}")).await);
    }
    new_order(&pool, 222, "CQforeign001").await;
    let first = apply(&pool, &ids[0], Event::LinkAccepted, OrderPatch::default()).await;
    apply(
        &pool,
        &first,
        Event::CheckActivated { fully_funded: true },
        OrderPatch {
            intake_amount: Some(dec!(25)),
            ..OrderPatch::default()
        },
    )
    .await;

    let mut conn = pool.acquire().await.unwrap();
    let page = client::history(&mut conn, CLIENT, 2, 0).await.unwrap();
    assert_eq!(
        page.iter()
            .map(|r| r.public_id.as_str())
            .collect::<Vec<_>>(),
        vec![ids[2].public_id.as_str(), ids[1].public_id.as_str()]
    );
    let rest = client::history(&mut conn, CLIENT, 2, 2).await.unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].public_id, ids[0].public_id);
    assert_eq!(rest[0].state, OrderState::Received);
    assert_eq!(rest[0].intake_amount, Some(dec!(25)));
    assert_eq!(rest[0].asset, Asset::Usdt);
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn active_ban_ignores_lifted_and_expired(pool: PgPool) {
    setup(&pool).await;
    sqlx::query("INSERT INTO staff (tg_user_id, role, display_name) VALUES (1, 'owner', 'owner')")
        .execute(&pool)
        .await
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(client::active_ban(&mut conn, CLIENT).await.unwrap(), None);

    sqlx::query(
        "INSERT INTO bans (user_id, reason, banned_by, expires_at, lifted_at) VALUES ($1, 'old one', 1, NULL, now())",
    )
    .bind(CLIENT)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(client::active_ban(&mut conn, CLIENT).await.unwrap(), None);

    let until = Utc::now() + Duration::hours(1);
    sqlx::query(
        "INSERT INTO bans (user_id, reason, banned_by, expires_at) VALUES ($1, 'serial failed checks', 1, $2)",
    )
    .bind(CLIENT)
    .bind(until)
    .execute(&pool)
    .await
    .unwrap();
    let ban = client::active_ban(&mut conn, CLIENT)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ban.reason, "serial failed checks");
    assert!(ban.expires_at.is_some());

    sqlx::query("UPDATE bans SET expires_at = now() - interval '1 minute' WHERE lifted_at IS NULL")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        client::active_ban(&mut conn, CLIENT).await.unwrap(),
        None,
        "истёкший бан не действует"
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn live_order_for_check_and_resync_selection(pool: PgPool) {
    setup(&pool).await;
    let live = new_order(&pool, CLIENT, "CQliveorder1").await;
    let live = apply(&pool, &live, Event::LinkAccepted, OrderPatch::default()).await;
    let rejected = new_order(&pool, CLIENT, "CQrejected01").await;
    let rejected = apply(
        &pool,
        &rejected,
        Event::Rejected,
        OrderPatch {
            failure_code: Some("direction_closed"),
            ..OrderPatch::default()
        },
    )
    .await;

    let mut conn = pool.acquire().await.unwrap();
    let found = client::live_order_for_check(&mut conn, CB, "CQliveorder1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (found.order_id, found.user_id, found.public_id.as_str()),
        (live.id, CLIENT, live.public_id.as_str())
    );
    assert_eq!(
        client::live_order_for_check(&mut conn, CB, "CQrejected01")
            .await
            .unwrap(),
        None,
        "терминальная заявка не живая"
    );

    let since = Utc::now() - Duration::hours(1);
    let all = client::orders_to_resync(&mut conn, since, false, 100)
        .await
        .unwrap();
    assert_eq!(
        all,
        vec![live.id],
        "отказ при приёме без сообщения не берётся"
    );

    client::set_progress_message(
        &mut conn,
        rejected.id,
        ProgressMessage {
            chat_id: CLIENT,
            message_id: 5,
        },
    )
    .await
    .unwrap();
    let all = client::orders_to_resync(&mut conn, since, false, 100)
        .await
        .unwrap();
    assert_eq!(all, vec![live.id, rejected.id]);
    let missing = client::orders_to_resync(&mut conn, since, true, 100)
        .await
        .unwrap();
    assert_eq!(missing, vec![live.id]);
    let later = client::orders_to_resync(&mut conn, Utc::now() + Duration::hours(1), false, 100)
        .await
        .unwrap();
    assert_eq!(later, vec![live.id], "давно закрытые не берутся");
}
