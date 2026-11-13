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
    // «Пропустить ›» создало чек, а экран чека пришёл позже, чем ждал сценарий; история на
    // время недоступна.
    e.world.inject(
        e.ub,
        Call::CheckPress,
        Fault::DelayReply(Duration::from_millis(700)),
        1,
    );
    // Первые два чтения истории — проверки «чистого чата» перед активацией и перед выдачей:
    // пропускаем их, сбой — на догоне ответа и на поиске чека.
    e.world
        .inject(e.ub, Call::History, Fault::DelayReply(Duration::ZERO), 2);
    e.world.inject(e.ub, Call::History, Fault::Disconnected, 3);
    e.drain().await;

    let (order_now, _) = e.order(order.id).await;
    assert_eq!(order_now.state, OrderState::PayoutPending, "ждёт сверку");
    let payout = e.ops(order.id).await.pop().unwrap();
    assert_eq!(payout.status, OpStatus::Unknown);
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1);

    // Экран чека дошёл.
    tokio::time::sleep(Duration::from_millis(800)).await;
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
    assert!(d.manual_reason.unwrap().contains("не хватает денег"));
    assert!(e.world.checks_created_by(e.ub).is_empty());
    let mut conn = e.pool.acquire().await.unwrap();
    let dir = storage::directions::get(&mut conn, Direction::CbToXrCheck)
        .await
        .unwrap();
    assert!(dir.auto_paused);
    // Деньги клиента в долге (payable), пока оператор не решит.
    assert_eq!(e.ledger("liability:payable:USDT").await, dec!(-100));
}

/// Клиент прислал чек xRocket в другой монете (живой ответ: «💰 Вы успешно получили 3 XROCK
/// (0.09$).»). Монеты в кошельке, обменять их нельзя: заявка человеку, выплаты нет, и это
/// знакомый ответ — направление не встаёт на паузу.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn check_in_another_coin_goes_to_a_human_without_pausing(pool: PgPool) {
    let e = env(pool).await;
    let url = e.world.client_check(XR, usdt(dec!(3))).unwrap();
    let order = accepted(e.submit(&url).await);
    e.world.inject(
        e.ub,
        Call::SendText,
        Fault::UnknownReply("💰 Вы успешно получили 3\u{a0}XROCK (0.09$).".to_owned()),
        1,
    );
    e.drain().await;
    let (order, d) = e.order(order.id).await;
    assert_eq!(order.state, OrderState::ManualReview);
    let reason = d.manual_reason.unwrap();
    assert!(reason.contains("XROCK"), "{reason}");
    assert!(e.world.checks_created_by(e.ub).is_empty(), "выплаты нет");
    let mut conn = e.pool.acquire().await.unwrap();
    let dir = storage::directions::get(&mut conn, Direction::XrToCbCheck)
        .await
        .unwrap();
    assert!(!dir.auto_paused, "знакомый ответ — не пауза");
    assert_eq!(e.ledger("liability:payable:USDT").await, dec!(0));
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
            .contains("исход активации неизвестен")
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

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn daily_limit_caps_a_new_client(pool: PgPool) {
    let e = env(pool).await;
    let mut conn = e.pool.acquire().await.unwrap();
    storage::settings::set(
        &mut conn,
        "daily_limit_usd",
        &serde_json::json!({ "default": 1000, "new_user": 200 }),
        None,
    )
    .await
    .unwrap();
    drop(conn);
    // Каждая заявка нового клиента удерживает до 100; третья не помещается в 200 за сутки.
    for _ in 0..2 {
        let url = e.world.client_check(CB, usdt(dec!(50))).unwrap();
        accepted(e.submit(&url).await);
    }
    let url = e.world.client_check(CB, usdt(dec!(50))).unwrap();
    assert!(matches!(
        e.submit(&url).await,
        Submitted::Rejected {
            reason: SubmitReject::DailyLimit,
            ..
        }
    ));
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn kill_switch_stops_new_checks_and_queued_payouts(pool: PgPool) {
    let e = env(pool).await;
    // Движок, который берёт по одной операции за вызов, с теми же юзерботами.
    let mut one = engine::Engine::new(
        e.pool.clone(),
        engine::EngineConfig {
            batch: 1,
            unavailable_backoff: Duration::ZERO,
            ..engine::EngineConfig::default()
        },
    );
    for w in e.wallets.values() {
        one = one.with_wallet(w.clone() as std::sync::Arc<dyn WalletBot>);
    }
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    let order = accepted(e.submit(&url).await);
    assert_eq!(one.process_due().await.unwrap(), 1, "только активация");
    let (now, _) = e.order(order.id).await;
    assert_eq!(now.state, OrderState::PayoutPending);

    let set = |on: bool| {
        let pool = e.pool.clone();
        async move {
            let mut conn = pool.acquire().await.unwrap();
            storage::settings::set(
                &mut conn,
                "kill_switch_payouts",
                &serde_json::json!({ "on": on }),
                None,
            )
            .await
            .unwrap();
        }
    };
    set(true).await;
    // Выплата в очереди не выполняется, попытка не тратится; новые чеки не принимаются.
    assert_eq!(one.process_due().await.unwrap(), 1);
    assert!(e.world.checks_created_by(e.ub).is_empty());
    let payout = e.ops(order.id).await.pop().unwrap();
    assert_eq!((payout.status, payout.attempts), (OpStatus::Pending, 0));
    let url = e.world.client_check(CB, usdt(dec!(10))).unwrap();
    assert!(matches!(
        e.submit(&url).await,
        Submitted::Rejected {
            reason: SubmitReject::ServicePaused,
            ..
        }
    ));

    set(false).await;
    one.process_due().await.unwrap();
    let (now, _) = e.order(order.id).await;
    assert_eq!(now.state, OrderState::Completed);
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1);
}

// ============================== находки проверки денежного ядра ==============================

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_second_engine_refuses_to_run(pool: PgPool) {
    let e = env(pool).await;
    let cancel = tokio_util::sync::CancellationToken::new();
    let first = {
        let cancel = cancel.clone();
        let engine = engine::Engine::new(e.pool.clone(), engine::EngineConfig::default());
        tokio::spawn(async move { engine.run(cancel).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let second = engine::Engine::new(e.pool.clone(), engine::EngineConfig::default());
    let refused = second.run(tokio_util::sync::CancellationToken::new()).await;
    assert!(
        matches!(refused, Err(engine::EngineError::AnotherInstance)),
        "{refused:?}"
    );
    cancel.cancel();
    first.await.unwrap().unwrap();
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn operation_of_another_wallet_account_goes_to_a_human(pool: PgPool) {
    let e = env(pool).await;
    let url = e.world.client_check(CB, usdt(dec!(20))).unwrap();
    let order = accepted(e.submit(&url).await);
    // Аккаунт CryptoBot заменили запасным, пока активация ждала в очереди.
    let mut conn = e.pool.acquire().await.unwrap();
    let spare = storage::wallets::ensure_userbot(&mut conn, "ub-2", "+00******02")
        .await
        .unwrap();
    sqlx::query("UPDATE wallet_accounts SET is_active = FALSE WHERE platform = 'cryptobot'")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("INSERT INTO wallet_accounts (platform, label, userbot_id) VALUES ('cryptobot', 'cb-2', $1)")
        .bind(spare)
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);
    e.drain().await;
    let (order, d) = e.order(order.id).await;
    assert_eq!(order.state, OrderState::ManualReview);
    assert!(
        d.manual_reason
            .unwrap()
            .contains("принадлежит аккаунту кошелька")
    );
    assert_eq!(
        e.world.balance(e.ub, CB, Asset::Usdt),
        dec!(1000),
        "чек не активировали"
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn refund_money_is_held_on_the_source_wallet(pool: PgPool) {
    let e = env(pool).await;
    // Выше лимита нового клиента (100): возврат 150 на CryptoBot.
    let url = e.world.client_check(CB, usdt(dec!(150))).unwrap();
    let order = accepted(e.submit(&url).await);
    let mut conn = e.pool.acquire().await.unwrap();
    // Только активация: выплату-возврат пока не выполняем.
    let op = storage::operations::claim_due(&mut conn, 1)
        .await
        .unwrap()
        .pop()
        .unwrap();
    storage::operations::release(&mut conn, op.id, chrono::Utc::now(), "test")
        .await
        .unwrap();
    drop(conn);
    let mut one = engine::Engine::new(
        e.pool.clone(),
        engine::EngineConfig {
            batch: 1,
            ..engine::EngineConfig::default()
        },
    );
    for w in e.wallets.values() {
        one = one.with_wallet(w.clone() as std::sync::Arc<dyn WalletBot>);
    }
    one.process_due().await.unwrap();
    let (now, _) = e.order(order.id).await;
    assert_eq!(now.state, OrderState::RefundPending);
    let mut conn = e.pool.acquire().await.unwrap();
    let (cb, _) = storage::wallets::active(&mut conn, CB)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        storage::holds::available(&mut conn, cb, Asset::Usdt)
            .await
            .unwrap(),
        dec!(1000),
        "150 пришли, но удержаны под возврат"
    );
    drop(conn);
    e.drain().await;
    let (now, _) = e.order(order.id).await;
    assert_eq!(now.state, OrderState::Refunded);
    let mut conn = e.pool.acquire().await.unwrap();
    assert_eq!(
        storage::holds::active(&mut conn, order.id).await.unwrap(),
        None
    );
    assert_eq!(
        storage::holds::available(&mut conn, cb, Asset::Usdt)
            .await
            .unwrap(),
        dec!(1000)
    );
    drop(conn);
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_repeated_already_activated_reply_never_writes_the_money_off(pool: PgPool) {
    let e = env(pool).await;
    let url = e.world.client_check(CB, usdt(dec!(40))).unwrap();
    let order = accepted(e.submit(&url).await);
    // Первая попытка: чек зачислен, бот промолчал. Вторая — новым random_id (как после
    // потери дедупликации) получила бы «уже активирован»: эмулируем вторую попытку напрямую.
    e.world.inject(e.ub, Call::SendText, Fault::NoReply, 1);
    let mut conn = e.pool.acquire().await.unwrap();
    let op = storage::operations::claim_due(&mut conn, 1)
        .await
        .unwrap()
        .pop()
        .unwrap();
    drop(conn);
    let tag = userbot::OpTag {
        key: op.idempotency_key.clone(),
        random_id: op.random_id,
    };
    let first = e.wallets[&CB]
        .activate_check(op.start_param.as_deref().unwrap(), &tag)
        .await
        .unwrap();
    assert!(matches!(first, userbot::ActivationOutcome::Unknown { .. }));
    let mut conn = e.pool.acquire().await.unwrap();
    assert!(
        storage::operations::retry(&mut conn, op.id, chrono::Utc::now(), "test")
            .await
            .unwrap()
    );
    // Вторая попытка с другим random_id: Telegram не узнает повтор, бот скажет «уже активирован».
    sqlx::query(
        "UPDATE operations SET request = jsonb_set(request, '{random_id}', '777') WHERE id = $1",
    )
    .bind(op.id)
    .execute(&mut *conn)
    .await
    .unwrap();
    drop(conn);
    tokio::time::sleep(Duration::from_millis(600)).await;
    e.drain().await;
    let (order, d) = e.order(order.id).await;
    assert_eq!(order.state, OrderState::ManualReview, "не IntakeFailed");
    assert!(
        d.manual_reason
            .unwrap()
            .contains("возможно, его забрала наша прежняя попытка")
    );
    assert_eq!(
        e.world.balance(e.ub, CB, Asset::Usdt),
        dec!(1040),
        "деньги у нас"
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn one_check_never_settles_two_operations(pool: PgPool) {
    let e = env(pool).await;
    let mut conn = e.pool.acquire().await.unwrap();
    let mut ids = Vec::new();
    for check in ["CQaaaaaaaaaa", "CQbbbbbbbbbb"] {
        let o = storage::orders::insert(
            &mut conn,
            &storage::orders::NewOrder {
                user_id: CLIENT,
                direction: Direction::CbToXrCheck,
                asset: Asset::Usdt,
                intake_start_param: Some(check),
                invoice_start_param: None,
            },
        )
        .await
        .unwrap();
        let (xr, _) = storage::wallets::active(&mut conn, XR)
            .await
            .unwrap()
            .unwrap();
        let id = storage::operations::insert(
            &mut conn,
            &storage::operations::NewOperation {
                order_id: o.id,
                kind: domain::fsm::OpKind::CreatePayoutCheck,
                role: domain::fsm::MoneyRole::Settle,
                platform: XR,
                wallet_account_id: xr,
                idempotency_key: &format!("ord-{}-payout-1", o.id),
                start_param: None,
                amount: Some(dec!(10)),
                asset: Asset::Usdt,
                random_id: 1 + o.id,
            },
        )
        .await
        .unwrap();
        ids.push(id);
    }
    sqlx::query("UPDATE operations SET status = 'dispatched'")
        .execute(&mut *conn)
        .await
        .unwrap();
    let ok = storage::operations::finish(
        &mut conn,
        ids[0],
        OpStatus::Succeeded,
        &serde_json::json!({}),
        Some("t_AbCdEfGhIjKlMnO"),
        None,
    )
    .await;
    assert!(ok.is_ok());
    let twice = storage::operations::finish(
        &mut conn,
        ids[1],
        OpStatus::Succeeded,
        &serde_json::json!({}),
        Some("t_AbCdEfGhIjKlMnO"),
        None,
    )
    .await;
    assert!(
        matches!(twice, Err(storage::StorageError::CheckAlreadyUsed(_))),
        "{twice:?}"
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn restricted_account_pauses_the_platform_and_calls_a_human(pool: PgPool) {
    let e = env(pool).await;
    let url = e.world.client_check(CB, usdt(dec!(20))).unwrap();
    let order = accepted(e.submit(&url).await);
    e.world.inject(
        e.ub,
        Call::SendText,
        Fault::Rpc {
            code: 400,
            name: "PEER_FLOOD".to_owned(),
        },
        1,
    );
    e.drain().await;
    let (order, d) = e.order(order.id).await;
    assert_eq!(order.state, OrderState::ManualReview);
    assert!(d.manual_reason.unwrap().contains("не может действовать"));
    let mut conn = e.pool.acquire().await.unwrap();
    for dir in [Direction::CbToXrCheck, Direction::XrToCbCheck] {
        let d = storage::directions::get(&mut conn, dir).await.unwrap();
        assert!(d.auto_paused, "{dir:?}");
    }
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn settlement_error_keeps_the_intake_and_calls_a_human(pool: PgPool) {
    let e = env(pool).await;
    let url = e.world.client_check(CB, usdt(dec!(30))).unwrap();
    let order = accepted(e.submit(&url).await);
    // Невозможный целевой резерв ломает расчёт комиссии.
    let mut conn = e.pool.acquire().await.unwrap();
    let (xr, _) = storage::wallets::active(&mut conn, XR)
        .await
        .unwrap()
        .unwrap();
    sqlx::query(
        "INSERT INTO reserve_thresholds (wallet_account_id, asset, target, low, critical, high) VALUES ($1, 'USDT', 0, 0, 0, 0)",
    )
    .bind(xr)
    .execute(&mut *conn)
    .await
    .unwrap();
    drop(conn);
    e.drain().await;
    let (order, d) = e.order(order.id).await;
    assert_eq!(order.state, OrderState::ManualReview);
    assert_eq!(d.intake_amount, Some(dec!(30)), "зачисление не откатилось");
    assert!(d.manual_reason.unwrap().contains("расчёт не удался"));
    e.assert_books_match().await;
}

/// Аудит 11.11.2026, М-2. Сбой базы внутри цикла движка: блокировка «один движок» не должна
/// пережить ошибку — иначе перезапущенный в том же процессе движок отвечает «уже запущен
/// другой экземпляр», супервизор сдаётся и обменник выключается.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn engine_restarts_after_a_database_error(pool: PgPool) {
    let e = env(pool).await;
    // В пуле несколько свободных соединений, как в работающем процессе (бот, пульт, пульс).
    {
        let a = e.pool.acquire().await.unwrap();
        let b = e.pool.acquire().await.unwrap();
        let c = e.pool.acquire().await.unwrap();
        drop((a, b, c));
    }
    sqlx::query("ALTER TABLE operations RENAME TO operations_gone")
        .execute(&e.pool)
        .await
        .unwrap();
    let engine = engine::Engine::new(e.pool.clone(), engine::EngineConfig::default());
    let failed = engine.run(tokio_util::sync::CancellationToken::new()).await;
    assert!(
        matches!(failed, Err(engine::EngineError::Storage(_))),
        "{failed:?}"
    );
    sqlx::query("ALTER TABLE operations_gone RENAME TO operations")
        .execute(&e.pool)
        .await
        .unwrap();
    for attempt in 1..=5 {
        let cancel = tokio_util::sync::CancellationToken::new();
        let stop = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            stop.cancel();
        });
        let again = engine.run(cancel).await;
        assert!(again.is_ok(), "restart {attempt}: {again:?}");
    }
}

/// Аудит 11.11.2026, М-1. FLOOD_WAIT на команде меню чеков после метки операции: сумма не
/// отправлялась, чек появиться не мог. Раньше заявка уходила к человеку, а выплаты кошелька
/// вставали на паузу; теперь повтор проходит меню заново и заявка завершается сама.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn menu_flood_wait_before_the_amount_does_not_need_a_human(pool: PgPool) {
    let e = env(pool).await;
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    let order = accepted(e.submit(&url).await);
    // 1-й SendText — /start чека, 2-й — метка операции в «Избранном», 3-й — /cheques.
    e.world
        .inject(e.ub, Call::SendText, Fault::DelayReply(Duration::ZERO), 2);
    e.world
        .inject(e.ub, Call::SendText, Fault::FloodWait(Duration::ZERO), 1);
    e.drain().await;

    let (order_now, d) = e.order(order.id).await;
    assert_eq!(order_now.state, OrderState::Completed);
    let checks = e.world.checks_created_by(e.ub);
    assert_eq!(checks.len(), 1, "ровно один чек");
    assert_eq!(d.payout_check_url.as_deref(), Some(checks[0].url.as_str()));
    let paused: i64 = sqlx::query_scalar("SELECT count(*) FROM directions WHERE auto_paused")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(paused, 0, "выплаты не встали на паузу");
    e.assert_books_match().await;
}

/// М-4 (шаг 1.7): чек активирован, а база падает на переходе заявки. Сколько пришло — уже
/// записано в операции; заявка доводится сама, кошелёк второй раз не спрашивают.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn database_failure_right_after_activation_keeps_the_amount_and_finishes_itself(
    pool: PgPool,
) {
    let e = env(pool).await;
    // Сбой связи с базой на первых четырёх проводках: три попытки сразу и ещё одна потом.
    sqlx::raw_sql(
        "CREATE SEQUENCE zz_outage;
         CREATE FUNCTION zz_outage() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF nextval('zz_outage') <= 4 THEN
             RAISE EXCEPTION 'injected outage' USING ERRCODE = '08006';
           END IF;
           RETURN NEW;
         END $$;
         CREATE TRIGGER zz_outage BEFORE INSERT ON ledger_transactions
           FOR EACH ROW EXECUTE FUNCTION zz_outage();",
    )
    .execute(&e.pool)
    .await
    .unwrap();
    let url = e.world.client_check(XR, usdt(dec!(30))).unwrap();
    let order = accepted(e.submit(&url).await);
    e.drain().await;
    let id = order.id;
    assert_eq!(
        e.order(id).await.0.state,
        OrderState::IntakePending,
        "заявка ждёт, а не ушла человеку"
    );
    let (status, saved): (String, Option<serde_json::Value>) = sqlx::query_as(
        "SELECT status, request -> 'activated' FROM operations WHERE order_id = $1 AND kind = 'activate_check'",
    )
    .bind(id)
    .fetch_one(&e.pool)
    .await
    .unwrap();
    assert_eq!(status, "pending");
    let saved = saved.expect("сколько пришло — записано в операции");
    assert!(saved.to_string().contains("30"), "{saved}");

    sqlx::query("UPDATE operations SET next_attempt_at = now() WHERE order_id = $1")
        .bind(id)
        .execute(&e.pool)
        .await
        .unwrap();
    e.drain().await;
    let (o, d) = e.order(id).await;
    assert_eq!(o.state, OrderState::Completed);
    assert_eq!(d.intake_amount, Some(dec!(30)));
    let activations = e
        .world
        .messages(e.ub, userbot::Chat::WalletBot(XR))
        .iter()
        .filter(|m| m.out && m.text.starts_with("/start"))
        .count();
    assert_eq!(activations, 1, "кошелёк второй раз не спрашивали");
    e.assert_books_match().await;
}

/// М-4 (шаг 1.7): итог активации записан, а процесс упал, не доведя заявку (операция осталась
/// `dispatched`). После перезапуска заявка доводится по записанному итогу, без повторной
/// активации — повтор сказал бы «чек уже активирован», и заявка ушла бы человеку.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn saved_activation_result_finishes_the_order_after_a_restart(pool: PgPool) {
    let e = env(pool).await;
    let url = e.world.client_check(XR, usdt(dec!(12))).unwrap();
    let order = accepted(e.submit(&url).await);
    let id = order.id;
    // Как будто воркер взял операцию, кошелёк ответил «пришло 12», итог записан — и процесс упал.
    let op = {
        let mut conn = e.pool.acquire().await.unwrap();
        storage::operations::claim_due(&mut conn, 1)
            .await
            .unwrap()
            .pop()
            .unwrap()
    };
    let tag = userbot::OpTag {
        key: op.idempotency_key.clone(),
        random_id: op.random_id,
    };
    let outcome = e.wallets[&XR]
        .activate_check(op.start_param.as_deref().unwrap(), &tag)
        .await
        .unwrap();
    assert!(matches!(outcome, userbot::ActivationOutcome::Received(_)));
    {
        let mut conn = e.pool.acquire().await.unwrap();
        storage::operations::save_activation(
            &mut conn,
            op.id,
            &serde_json::to_value(&outcome).unwrap(),
        )
        .await
        .unwrap();
    }
    e.engine.recover().await.unwrap();
    e.drain().await;
    let (o, d) = e.order(id).await;
    assert_eq!(o.state, OrderState::Completed);
    assert_eq!(d.intake_amount, Some(dec!(12)));
    let activations = e
        .world
        .messages(e.ub, userbot::Chat::WalletBot(XR))
        .iter()
        .filter(|m| m.out && m.text.starts_with("/start"))
        .count();
    assert_eq!(activations, 1, "кошелёк второй раз не спрашивали");
    e.assert_books_match().await;
}
