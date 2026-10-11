//! Движок + юзербот на симуляторе + живой PostgreSQL: заявка от ссылки до выплаты.
//! После каждого сценария: леджер сбалансирован (триггер), балансы кошельков в симуляторе
//! равны балансам по леджеру, на операцию — не больше одного созданного чека.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::{CB, CLIENT, XR, accepted, env, usdt};
use domain::fsm::OrderState;
use domain::{Asset, Direction};
use engine::{SubmitReject, Submitted};
use rust_decimal_macros::dec;
use sqlx::PgPool;
use storage::operations::OpStatus;
use userbot::WalletBot;
use userbot::sim::{Call, Fault};

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn cryptobot_check_becomes_an_xrocket_check(pool: PgPool) {
    let e = env(pool).await;
    let mut updates = e.engine.subscribe();
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    let order = accepted(e.submit(&url).await);
    assert_eq!(order.state, OrderState::IntakePending);
    e.drain().await;

    let (order, d) = e.order(order.id).await;
    assert_eq!(order.state, OrderState::Completed);
    assert_eq!(d.intake_amount, Some(dec!(100)));
    assert_eq!(d.fee_amount, Some(dec!(2.5)));
    assert_eq!(d.payout_amount, Some(dec!(97.5)));
    let check_url = d.payout_check_url.expect("ссылка на чек выплаты");
    assert!(check_url.starts_with("https://t.me/xrocket?start=t_"));

    assert_eq!(e.ledger("liability:payable:USDT").await, dec!(0));
    assert_eq!(e.ledger("revenue:fee:USDT").await, dec!(-2.5));
    assert_eq!(e.ledger("asset:cb:USDT").await, dec!(1100));
    assert_eq!(e.ledger("asset:xr:USDT").await, dec!(902.5));
    e.assert_books_match().await;

    // Клиент забирает чек.
    e.world.claim_check(e.client, &check_url).unwrap();
    assert_eq!(e.world.balance(e.client, XR, Asset::Usdt), dec!(97.5));
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1);

    let mut states = Vec::new();
    while let Ok(u) = updates.try_recv() {
        states.push(u.state);
    }
    assert_eq!(states.first(), Some(&OrderState::IntakePending));
    assert_eq!(states.last(), Some(&OrderState::Completed));

    let mut conn = e.pool.acquire().await.unwrap();
    let held = storage::holds::active(&mut conn, order.id).await.unwrap();
    assert_eq!(held, None, "удержание закрыто");
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn xrocket_check_becomes_a_cryptobot_check(pool: PgPool) {
    let e = env(pool).await;
    let url = e.world.client_check(XR, usdt(dec!(50))).unwrap();
    let order = accepted(e.submit(&url).await);
    e.drain().await;
    let (order, d) = e.order(order.id).await;
    assert_eq!(order.state, OrderState::Completed);
    assert_eq!(d.fee_amount, Some(dec!(0.75)));
    assert_eq!(d.payout_amount, Some(dec!(49.25)));
    assert!(
        d.payout_check_url
            .unwrap()
            .starts_with("https://t.me/send?start=CQ")
    );
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn rejected_check_releases_the_hold_and_posts_nothing(pool: PgPool) {
    let e = env(pool).await;
    let url = e.world.client_check(CB, usdt(dec!(10))).unwrap();
    e.world.claim_check(e.client, &url).unwrap();
    let order = accepted(e.submit(&url).await);
    e.drain().await;
    let (order, d) = e.order(order.id).await;
    assert_eq!(order.state, OrderState::IntakeFailed);
    assert_eq!(d.failure_code.as_deref(), Some("check_already_activated"));
    assert_eq!(e.ledger("asset:cb:USDT").await, dec!(1000));
    let mut conn = e.pool.acquire().await.unwrap();
    assert_eq!(
        storage::holds::active(&mut conn, order.id).await.unwrap(),
        None
    );
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn check_above_the_new_client_limit_is_refunded(pool: PgPool) {
    let e = env(pool).await;
    let url = e.world.client_check(CB, usdt(dec!(150))).unwrap();
    let order = accepted(e.submit(&url).await);
    e.drain().await;
    let (order, d) = e.order(order.id).await;
    assert_eq!(order.state, OrderState::Refunded);
    assert_eq!(d.refund_amount, Some(dec!(150)));
    assert_eq!(d.failure_code.as_deref(), Some("above_maximum"));
    let refund_url = d.payout_check_url.unwrap();
    assert!(
        refund_url.starts_with("https://t.me/send?start=CQ"),
        "возврат — на платформе входа"
    );
    assert_eq!(e.ledger("asset:cb:USDT").await, dec!(1000));
    assert_eq!(e.ledger("liability:payable:USDT").await, dec!(0));
    e.assert_books_match().await;
    e.world.claim_check(e.client, &refund_url).unwrap();
    assert_eq!(e.world.balance(e.client, CB, Asset::Usdt), dec!(150));
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn submissions_are_refused_without_touching_money(pool: PgPool) {
    let e = env(pool).await;
    let url = e.world.client_check(CB, usdt(dec!(10))).unwrap();
    accepted(e.submit(&url).await);
    assert!(matches!(
        e.submit(&url).await,
        Submitted::Rejected {
            reason: SubmitReject::AlreadyInWork,
            order: None
        }
    ));

    let mut conn = e.pool.acquire().await.unwrap();
    storage::directions::set_open(&mut conn, Direction::XrToCbCheck, false)
        .await
        .unwrap();
    let xr_url = e.world.client_check(XR, usdt(dec!(10))).unwrap();
    let Submitted::Rejected {
        reason: SubmitReject::DirectionClosed,
        order: Some(order),
    } = e.submit(&xr_url).await
    else {
        panic!("закрытое направление");
    };
    assert_eq!(order.state, OrderState::Rejected);

    // Резерв xRocket почти весь удержан: новая заявка CB → XR не помещается.
    let big = storage::orders::insert(
        &mut conn,
        &storage::orders::NewOrder {
            user_id: CLIENT,
            direction: Direction::CbToXrCheck,
            asset: Asset::Usdt,
            intake_start_param: Some("CQholdholdho"),
            invoice_start_param: None,
        },
    )
    .await
    .unwrap();
    let (xr_wallet, _) = storage::wallets::active(&mut conn, XR)
        .await
        .unwrap()
        .unwrap();
    let free = storage::holds::available(&mut conn, xr_wallet, Asset::Usdt)
        .await
        .unwrap();
    storage::holds::place(&mut conn, big.id, xr_wallet, Asset::Usdt, free - dec!(1))
        .await
        .unwrap();
    let url = e.world.client_check(CB, usdt(dec!(10))).unwrap();
    assert!(matches!(
        e.submit(&url).await,
        Submitted::Rejected {
            reason: SubmitReject::NoReserve,
            ..
        }
    ));
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn payout_timeout_is_reconciled_to_exactly_one_check(pool: PgPool) {
    let e = env(pool).await;
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    let order = accepted(e.submit(&url).await);
    // Активация проходит обычным путём; сбои — только на выдаче чека.
    assert_eq!(e.engine.process_due().await.unwrap(), 1);
    // Чек создан, ответ потерян, повтор не дошёл, история на время недоступна.
    e.world
        .inject(e.ub, Call::SendInline, Fault::TimeoutAfterEffect, 1);
    e.world
        .inject(e.ub, Call::SendInline, Fault::Disconnected, 1);
    // Первое чтение истории — проверка «чистого чата» перед выдачей: пропускаем его.
    e.world
        .inject(e.ub, Call::History, Fault::DelayReply(Duration::ZERO), 1);
    e.world.inject(e.ub, Call::History, Fault::Disconnected, 3);
    e.drain().await;

    let (order_now, _) = e.order(order.id).await;
    assert_eq!(order_now.state, OrderState::PayoutPending, "ждёт сверку");
    let payout = e.ops(order.id).await.pop().unwrap();
    assert_eq!(payout.status, OpStatus::Unknown);
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1);

    e.world.clear_faults(e.ub);
    e.engine.resolve_unknown().await.unwrap();
    let (order_now, d) = e.order(order.id).await;
    assert_eq!(order_now.state, OrderState::Completed);
    assert_eq!(
        d.payout_check_url.as_deref(),
        Some(e.world.checks_created_by(e.ub)[0].url.as_str())
    );
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1, "второго чека нет");
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn wallet_short_of_funds_goes_to_a_human_and_pauses_the_direction(pool: PgPool) {
    let e = env(pool).await;
    // Расхождение: по леджеру 1000, на кошельке xRocket — 5.
    e.world.set_balance(e.ub, XR, Asset::Usdt, dec!(5)).unwrap();
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    let order = accepted(e.submit(&url).await);
    e.drain().await;
    let (order, d) = e.order(order.id).await;
    assert_eq!(order.state, OrderState::ManualReview);
    assert!(d.manual_reason.unwrap().contains("not enough funds"));
    assert!(e.world.checks_created_by(e.ub).is_empty());
    let mut conn = e.pool.acquire().await.unwrap();
    let dir = storage::directions::get(&mut conn, Direction::CbToXrCheck)
        .await
        .unwrap();
    assert!(dir.auto_paused);
    // Деньги клиента в долге (payable), пока оператор не решит.
    assert_eq!(e.ledger("liability:payable:USDT").await, dec!(-100));
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn flood_wait_postpones_without_spending_an_attempt(pool: PgPool) {
    let e = env(pool).await;
    let url = e.world.client_check(CB, usdt(dec!(20))).unwrap();
    let order = accepted(e.submit(&url).await);
    e.world
        .inject(e.ub, Call::SendText, Fault::FloodWait(Duration::ZERO), 1);
    e.drain().await;
    let (order_now, _) = e.order(order.id).await;
    assert_eq!(order_now.state, OrderState::Completed);
    let activation = &e.ops(order.id).await[0];
    assert_eq!(activation.attempts, 1, "FLOOD_WAIT попытку не тратит");
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn silent_activation_is_asked_again_with_the_same_random_id(pool: PgPool) {
    let e = env(pool).await;
    let url = e.world.client_check(XR, usdt(dec!(30))).unwrap();
    let order = accepted(e.submit(&url).await);
    // Бот промолчал: деньги пришли, ответа нет. Повторы идут тем же random_id (второго
    // /start нет) и ищут ответ в истории; ответа не было вовсе — решает человек.
    e.world.inject(e.ub, Call::SendText, Fault::NoReply, 1);
    e.drain().await;
    let (order_now, d) = e.order(order.id).await;
    assert_eq!(order_now.state, OrderState::ManualReview);
    assert!(
        d.manual_reason
            .unwrap()
            .contains("activation outcome unknown")
    );
    let activation = &e.ops(order.id).await[0];
    assert_eq!(activation.status, OpStatus::Unknown);
    assert_eq!(activation.attempts, activation.max_attempts);
    let sent: Vec<_> = e
        .world
        .messages(e.ub, userbot::Chat::WalletBot(XR))
        .into_iter()
        .filter(|m| m.out)
        .collect();
    assert_eq!(sent.len(), 1, "повторы не шлют второй /start");
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn restart_mid_payout_is_reconciled_not_repeated(pool: PgPool) {
    let e = env(pool).await;
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    let order = accepted(e.submit(&url).await);
    // Активация проходит обычным путём.
    let mut conn = e.pool.acquire().await.unwrap();
    let ops = storage::operations::claim_due(&mut conn, 10).await.unwrap();
    assert_eq!(ops.len(), 1);
    drop(conn);
    // «Падение» сразу после того, как активацию взяли в работу: восстановление повторит её.
    e.engine.recover().await.unwrap();
    // Выплату взяли в работу и чек создали, а записать итог не успели.
    let mut conn = e.pool.acquire().await.unwrap();
    for _ in 0..5 {
        let claimed = storage::operations::claim_due(&mut conn, 10).await.unwrap();
        if claimed.is_empty() {
            break;
        }
        let op = &claimed[0];
        if op.kind == domain::fsm::OpKind::CreatePayoutCheck {
            let tag = userbot::OpTag {
                key: op.idempotency_key.clone(),
                random_id: op.random_id,
            };
            let out = e.wallets[&XR]
                .issue_check(&usdt(op.amount.unwrap()), &tag)
                .await
                .unwrap();
            assert!(matches!(out, userbot::IssueOutcome::Issued { .. }));
            break;
        }
        // Активация: выполнить как воркер (через recover/process_due нельзя — она уже взята).
        storage::operations::release(&mut conn, op.id, chrono::Utc::now(), "test")
            .await
            .unwrap();
        drop(conn);
        e.engine.process_due().await.unwrap();
        conn = e.pool.acquire().await.unwrap();
    }
    drop(conn);
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1);

    e.engine.recover().await.unwrap();
    let (order_now, d) = e.order(order.id).await;
    assert_eq!(order_now.state, OrderState::Completed);
    assert_eq!(
        d.payout_check_url.as_deref(),
        Some(e.world.checks_created_by(e.ub)[0].url.as_str())
    );
    assert_eq!(
        e.world.checks_created_by(e.ub).len(),
        1,
        "чек не создан второй раз"
    );
    e.assert_books_match().await;
}
