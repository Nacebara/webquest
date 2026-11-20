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

/// Веха 4.2: кошелёк нельзя перенести на другой аккаунт, пока его деньги в полёте — операция
/// без итога или удержание живой заявки; ответ пришёл бы старому аккаунту.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_wallet_with_money_in_flight_cannot_move(pool: PgPool) {
    setup(&pool).await;
    let mut conn = pool.acquire().await.unwrap();
    assert!(
        storage::wallets::move_blockers(&mut conn, XR)
            .await
            .unwrap()
            .is_empty()
    );
    drop(conn);
    let id = order(&pool, "CQmove1").await;
    let xr = wallet(&pool, XR).await;
    let mut conn = pool.acquire().await.unwrap();
    storage::holds::place(&mut conn, id, xr, Asset::Usdt, dec!(97.5))
        .await
        .unwrap();
    let op_id = storage::operations::insert(
        &mut conn,
        &op(id, OpKind::CreatePayoutCheck, "ord-move-payout", xr),
    )
    .await
    .unwrap();
    let blockers = storage::wallets::move_blockers(&mut conn, XR)
        .await
        .unwrap();
    assert_eq!(blockers.len(), 2, "{blockers:?}");
    assert!(
        blockers[0].contains("ord-move-payout (pending)"),
        "{blockers:?}"
    );
    assert!(blockers[1].contains("держит 97.5 USDT"), "{blockers:?}");
    // Другой кошелёк свободен.
    assert!(
        storage::wallets::move_blockers(&mut conn, CB)
            .await
            .unwrap()
            .is_empty()
    );
    // Неясный исход держит так же, как живая операция.
    sqlx::query("UPDATE operations SET status = 'unknown' WHERE id = $1")
        .bind(op_id)
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(
        storage::wallets::move_blockers(&mut conn, XR)
            .await
            .unwrap()
            .iter()
            .any(|b| b.contains("(unknown)"))
    );
}

/// Аудит 16.11 («Забрать» гаснет): уведомление «ваш чек забрали» не называет чек — отмечаем,
/// только если подходит ровно одна наша выплата той же платформы и суммы.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_claim_notice_marks_only_an_unambiguous_check(pool: PgPool) {
    setup(&pool).await;
    let xr = wallet(&pool, XR).await;
    let mut ids = Vec::new();
    for (n, check) in ["CQclaim1", "CQclaim2"].iter().enumerate() {
        let order_id = order(&pool, check).await;
        let mut conn = pool.acquire().await.unwrap();
        let key = format!("ord-claim-{n}-payout");
        let op_id = storage::operations::insert(
            &mut conn,
            &op(order_id, OpKind::CreatePayoutCheck, &key, xr),
        )
        .await
        .unwrap();
        sqlx::query(
            "UPDATE operations SET status = 'succeeded', finished_at = now() WHERE id = $1",
        )
        .bind(op_id)
        .execute(&mut *conn)
        .await
        .unwrap();
        ids.push(order_id);
    }
    let mut conn = pool.acquire().await.unwrap();
    // Две выплаты по 97.5 — какая забрана, неизвестно: не трогаем ни одну.
    assert_eq!(
        storage::operations::mark_claimed(&mut conn, XR, dec!(97.5), Asset::Usdt, None)
            .await
            .unwrap(),
        None
    );
    // Другая сумма — ничего.
    assert_eq!(
        storage::operations::mark_claimed(&mut conn, XR, dec!(10), Asset::Usdt, None)
            .await
            .unwrap(),
        None
    );
    // Одну уже отметили (например, прошлое уведомление) — оставшаяся однозначна.
    sqlx::query("UPDATE operations SET claimed_at = now() WHERE order_id = $1")
        .bind(ids[0])
        .execute(&mut *conn)
        .await
        .unwrap();
    assert_eq!(
        storage::operations::mark_claimed(&mut conn, XR, dec!(97.5), Asset::Usdt, None)
            .await
            .unwrap(),
        Some(ids[1])
    );
    assert!(
        storage::operations::settle_claimed(&mut conn, ids[1])
            .await
            .unwrap()
    );
}

/// П-13: предпросмотр стоп-крана говорит, сколько выплат встанет и на какую сумму. Считаются
/// только не ушедшие (`pending`) выводящие деньги операции; приём чека и уже отправленная
/// выплата — нет.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn queued_money_out_is_what_a_kill_switch_holds(pool: PgPool) {
    setup(&pool).await;
    let (cb, xr) = (wallet(&pool, CB).await, wallet(&pool, XR).await);
    let a = order(&pool, "CQqueuedAAAA").await;
    let b = order(&pool, "CQqueuedBBBB").await;
    let c = order(&pool, "CQqueuedCCCC").await;
    let mut conn = pool.acquire().await.unwrap();
    assert!(
        storage::operations::queued_money_out(&mut conn)
            .await
            .unwrap()
            .is_empty()
    );
    let mut refund = op(b, OpKind::CreateRefundCheck, "ord-2-refund", xr);
    refund.amount = Some(dec!(10));
    refund.random_id = 2;
    storage::operations::insert(&mut conn, &refund)
        .await
        .unwrap();
    let mut intake = op(c, OpKind::ActivateCheck, "ord-3-intake-1", cb);
    intake.start_param = Some("CQqueuedCCCC");
    intake.random_id = 3;
    storage::operations::insert(&mut conn, &intake)
        .await
        .unwrap();
    // Уже отправленная выплата: стоп-кран её не остановит.
    let mut sent = op(a, OpKind::CreatePayoutCheck, "ord-1-payout", xr);
    sent.random_id = 1;
    storage::operations::insert(&mut conn, &sent).await.unwrap();
    let claimed = storage::operations::claim_due(&mut conn, 10).await.unwrap();
    assert!(claimed.iter().any(|o| o.idempotency_key == "ord-1-payout"));
    sqlx::query("UPDATE operations SET status = 'pending' WHERE idempotency_key <> 'ord-1-payout'")
        .execute(&mut *conn)
        .await
        .unwrap();
    let queued = storage::operations::queued_money_out(&mut conn)
        .await
        .unwrap();
    assert_eq!(queued, vec![(Asset::Usdt, 1, dec!(10))]);
}

/// Д-19.9 (проверка 19.11): причин автопаузы у направления может быть несколько, у каждой — свой
/// кошелёк; снимается только своя. Раньше вторая причина не записывалась («уже на паузе»), а
/// снятие первой («без ответа») снимало паузу целиком — выплаты из кошелька с недостачей шли снова.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn an_auto_pause_is_lifted_only_with_its_own_reason(pool: PgPool) {
    use storage::directions::{
        Side, auto_pause_platform, clear_auto_pause, clear_auto_pause_reason,
    };
    let mut conn = pool.acquire().await.unwrap();
    let state = |d: storage::directions::DirectionRow| (d.auto_paused, d.pause_reason);
    async fn dir(conn: &mut sqlx::PgConnection, d: Direction) -> storage::directions::DirectionRow {
        storage::directions::get(conn, d).await.unwrap()
    }
    const UNANSWERED: &str = "команда кошельку без ответа";
    // xRocket не ответил на активацию: все четыре направления (xRocket в каждом) — на паузе.
    let mut paused = 0;
    for side in [Side::Source, Side::Target] {
        paused += auto_pause_platform(&mut conn, XR, side, UNANSWERED)
            .await
            .unwrap();
    }
    assert_eq!(paused, 4);
    // Сверка: в xRocket не хватает — выплаты из него на паузу; повтор сверки — та же причина.
    let short = |n: u32| format!("сверка: в кошельке 628 USDT, ожидалось 640 — не хватает {n}");
    assert_eq!(
        auto_pause_platform(&mut conn, XR, Side::Target, &short(12))
            .await
            .unwrap(),
        0,
        "уже на паузе — новых пауз нет"
    );
    auto_pause_platform(&mut conn, XR, Side::Target, &short(13))
        .await
        .unwrap();
    // Заявку без ответа разобрали: снята только «без ответа».
    clear_auto_pause_reason(&mut conn, XR, UNANSWERED)
        .await
        .unwrap();
    for d in [Direction::XrToCbCheck, Direction::PayCbInvoice] {
        assert_eq!(
            state(dir(&mut conn, d).await),
            (false, None),
            "{d:?}: приём из xRocket открыт"
        );
    }
    for d in [Direction::CbToXrCheck, Direction::PayXrInvoice] {
        assert_eq!(
            state(dir(&mut conn, d).await),
            (true, Some(short(13))),
            "{d:?}: выплаты в xRocket ждут человека по недостаче"
        );
    }
    // Человек снял паузу кнопкой — снято всё; снятие одной причины потом ничего не возвращает.
    storage::staff::ensure_owner(&mut conn, 1, "Владелец")
        .await
        .unwrap();
    for d in [Direction::CbToXrCheck, Direction::PayXrInvoice] {
        clear_auto_pause(&mut conn, d, 1).await.unwrap();
    }
    clear_auto_pause_reason(&mut conn, XR, &short(13))
        .await
        .unwrap();
    for d in Direction::ALL {
        assert!(!dir(&mut conn, d).await.auto_paused, "{d:?}");
    }

    // Оба кошелька без ответа (сбой Telegram): ответил xRocket — пауза CryptoBot остаётся.
    for p in [CB, XR] {
        for side in [Side::Source, Side::Target] {
            auto_pause_platform(&mut conn, p, side, UNANSWERED)
                .await
                .unwrap();
        }
    }
    clear_auto_pause_reason(&mut conn, XR, UNANSWERED)
        .await
        .unwrap();
    for d in Direction::ALL {
        assert!(
            dir(&mut conn, d).await.auto_paused,
            "{d:?}: CryptoBot всё ещё без ответа"
        );
    }
    clear_auto_pause_reason(&mut conn, CB, UNANSWERED)
        .await
        .unwrap();
    for d in Direction::ALL {
        assert!(!dir(&mut conn, d).await.auto_paused, "{d:?}");
    }
}

/// Д-19.7 (проверка 19.11): уведомление называет, кто забрал чек. Владелец забрал свой чек на ту же
/// сумму — выплата клиента не отмечается забранной («Забрать» у клиента остаётся); клиент забрал —
/// отмечается, по имени или @имени. Раньше сходилась одна сумма.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_claim_by_someone_else_does_not_mark_the_clients_check(pool: PgPool) {
    setup(&pool).await;
    let xr = wallet(&pool, XR).await;
    let mut orders = Vec::new();
    for (n, check) in ["CQname1", "CQname2"].iter().enumerate() {
        let order_id = order(&pool, check).await;
        let mut conn = pool.acquire().await.unwrap();
        let key = format!("ord-name-{n}-payout");
        let op_id = storage::operations::insert(
            &mut conn,
            &op(order_id, OpKind::CreatePayoutCheck, &key, xr),
        )
        .await
        .unwrap();
        sqlx::query(
            "UPDATE operations SET status = 'succeeded', finished_at = now() WHERE id = $1",
        )
        .bind(op_id)
        .execute(&mut *conn)
        .await
        .unwrap();
        orders.push(order_id);
    }
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("UPDATE users SET first_name = 'Ivan' WHERE tg_user_id = $1")
        .bind(CLIENT)
        .execute(&mut *conn)
        .await
        .unwrap();
    // Вторая выплата — другому клиенту.
    storage::users::upsert(
        &mut conn,
        222,
        &storage::users::Profile {
            first_name: Some("Oleg"),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE orders SET user_id = 222 WHERE id = $1")
        .bind(orders[1])
        .execute(&mut *conn)
        .await
        .unwrap();
    let mark = |by: &'static str| {
        let pool = pool.clone();
        async move {
            let mut conn = pool.acquire().await.unwrap();
            storage::operations::mark_claimed(&mut conn, XR, dec!(97.5), Asset::Usdt, Some(by))
                .await
                .unwrap()
        }
    };
    assert_eq!(
        mark("Игнат").await,
        None,
        "чек владельца — не выплата клиента"
    );
    assert_eq!(
        mark("Oleg").await,
        Some(orders[1]),
        "по имени — его выплата"
    );
    assert_eq!(mark("@client").await, Some(orders[0]), "по @имени");
    assert!(
        storage::operations::settle_claimed(&mut conn, orders[0])
            .await
            .unwrap()
    );
}
