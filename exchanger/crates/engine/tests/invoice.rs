//! Оплата чужого счёта (SPEC §4, П3–П7, П19): движок + юзербот на симуляторе + PostgreSQL.
//! После каждого сценария: леджер сбалансирован (триггер), балансы кошельков в симуляторе
//! равны балансам по леджеру, счёт оплачен не больше одного раза.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::{CB, CLIENT, Env, XR, env, env_without_payer, usdt};
use domain::fsm::{OpKind, OrderState};
use domain::{Asset, Decimal, Direction, Money};
use engine::{InvoiceIssue, InvoiceReject, InvoiceSubmitted, SubmitReject, Submitted};
use rust_decimal_macros::dec;
use sqlx::PgPool;
use storage::operations::OpStatus;
use storage::orders::OrderRow;
use userbot::WalletBot;
use userbot::sim::{Call, Fault, InvoiceSpec, SimInvoice, SimInvoiceStatus};

async fn open_invoices(e: &Env) {
    let mut conn = e.pool.acquire().await.unwrap();
    for d in [Direction::PayCbInvoice, Direction::PayXrInvoice] {
        storage::directions::set_open(&mut conn, d, true)
            .await
            .unwrap();
    }
}

/// Env с открытыми направлениями оплаты счетов.
async fn setup(pool: PgPool) -> Env {
    let e = env(pool).await;
    open_invoices(&e).await;
    e
}

fn shop_invoice(e: &Env, spec: InvoiceSpec) -> SimInvoice {
    let shop = e.world.add_account("shop");
    e.world.create_invoice(spec.by(shop)).unwrap()
}

async fn submit_invoice(e: &Env, inv: &SimInvoice) -> InvoiceSubmitted {
    let link = parsers::links::parse_url(&inv.url).unwrap();
    e.engine.submit_invoice(CLIENT, &link).await.unwrap()
}

async fn quoted(e: &Env, inv: &SimInvoice) -> OrderRow {
    match submit_invoice(e, inv).await {
        InvoiceSubmitted::Quoted(order) => order,
        other => panic!("{other:?}"),
    }
}

fn rejected(s: InvoiceSubmitted) -> InvoiceReject {
    match s {
        InvoiceSubmitted::Rejected(reason) => reason,
        other => panic!("{other:?}"),
    }
}

/// Клиент присылает чек платформы `platform` на `amount`; он уходит в заявку `order`.
async fn pay_in(e: &Env, order: &OrderRow, platform: domain::Platform, amount: Decimal) {
    let url = e.world.client_check(platform, usdt(amount)).unwrap();
    match e.submit(&url).await {
        Submitted::Accepted(o) => {
            assert_eq!(o.id, order.id, "чек ушёл в заявку на оплату счёта");
            assert_eq!(o.state, OrderState::IntakePending);
        }
        other => panic!("{other:?}"),
    }
}

async fn held(e: &Env, order: i64) -> Option<Decimal> {
    let mut conn = e.pool.acquire().await.unwrap();
    storage::holds::active(&mut conn, order)
        .await
        .unwrap()
        .map(|(_, _, amount)| amount)
}

async fn expire_quote(e: &Env, order: i64) {
    sqlx::query("UPDATE orders SET quote_expires_at = now() - interval '1 minute' WHERE id = $1")
        .bind(order)
        .execute(&e.pool)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn cryptobot_invoice_is_paid_with_an_xrocket_check(pool: PgPool) {
    let e = setup(pool).await;
    let inv = shop_invoice(&e, InvoiceSpec::crypto(CB, usdt(dec!(10))));
    let order = quoted(&e, &inv).await;
    assert_eq!(order.state, OrderState::AwaitingFunds);
    let (_, d) = e.order(order.id).await;
    assert_eq!(d.invoice_amount, Some(dec!(10)));
    // pay_cb_invoice: 3 %, минимум 0,20 → прислать 10,30.
    assert_eq!(d.quote_amount_in, Some(dec!(10.30)));
    assert_eq!(d.fee_amount, Some(dec!(0.30)));
    assert_eq!(
        held(&e, order.id).await,
        Some(dec!(10)),
        "резерв на CryptoBot"
    );

    pay_in(&e, &order, XR, dec!(10.30)).await;
    e.drain().await;

    let (order, _) = e.order(order.id).await;
    assert_eq!(order.state, OrderState::Completed);
    let paid = e.world.invoice(&inv.code).unwrap();
    assert_eq!(paid.status, SimInvoiceStatus::Paid);
    assert_eq!(paid.payments.len(), 1);
    assert_eq!(e.ledger("asset:xr:USDT").await, dec!(1010.30));
    assert_eq!(e.ledger("asset:cb:USDT").await, dec!(990));
    assert_eq!(e.ledger("revenue:fee:USDT").await, dec!(-0.30));
    assert_eq!(e.ledger("liability:payable:USDT").await, dec!(0));
    assert_eq!(held(&e, order.id).await, None, "удержание израсходовано");
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn xrocket_invoice_is_paid_with_a_cryptobot_check(pool: PgPool) {
    let e = setup(pool).await;
    let inv = shop_invoice(&e, InvoiceSpec::crypto(XR, usdt(dec!(20))));
    let order = quoted(&e, &inv).await;
    let (_, d) = e.order(order.id).await;
    // pay_xr_invoice: 2 % → 20,40.
    assert_eq!(d.quote_amount_in, Some(dec!(20.40)));
    pay_in(&e, &order, CB, dec!(20.40)).await;
    e.drain().await;
    let (order, _) = e.order(order.id).await;
    assert_eq!(order.state, OrderState::Completed);
    assert_eq!(e.world.invoice(&inv.code).unwrap().payments.len(), 1);
    assert_eq!(e.ledger("asset:cb:USDT").await, dec!(1020.40));
    assert_eq!(e.ledger("asset:xr:USDT").await, dec!(980));
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn underpaid_invoice_waits_for_the_rest(pool: PgPool) {
    let e = setup(pool).await;
    let inv = shop_invoice(&e, InvoiceSpec::crypto(CB, usdt(dec!(10))));
    let order = quoted(&e, &inv).await;

    pay_in(&e, &order, XR, dec!(7)).await;
    e.drain().await;
    let (o, d) = e.order(order.id).await;
    assert_eq!(o.state, OrderState::AwaitingFunds, "ждём остаток");
    assert_eq!(d.intake_amount, Some(dec!(7)));
    assert_eq!(e.ledger("asset:xr:USDT").await, dec!(1007));
    assert_eq!(
        e.world.invoice(&inv.code).unwrap().status,
        SimInvoiceStatus::Active
    );

    pay_in(&e, &order, XR, dec!(3.30)).await;
    e.drain().await;
    let (o, d) = e.order(order.id).await;
    assert_eq!(o.state, OrderState::Completed);
    assert_eq!(d.intake_amount, Some(dec!(10.30)));
    // Две проводки прихода — по сумме каждого чека, без задвоения.
    assert_eq!(e.ledger("asset:xr:USDT").await, dec!(1010.30));
    assert_eq!(e.ledger("liability:payable:USDT").await, dec!(0));
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn overpayment_comes_back_by_check(pool: PgPool) {
    let e = setup(pool).await;
    let inv = shop_invoice(&e, InvoiceSpec::crypto(CB, usdt(dec!(10))));
    let order = quoted(&e, &inv).await;
    pay_in(&e, &order, XR, dec!(15)).await;
    e.drain().await;

    let (o, d) = e.order(order.id).await;
    assert_eq!(o.state, OrderState::Completed);
    assert_eq!(d.refund_amount, Some(dec!(4.70)));
    let url = d.payout_check_url.expect("чек на переплату");
    assert!(url.starts_with("https://t.me/xrocket?start="), "{url}");
    let excess: Vec<_> = e
        .ops(order.id)
        .await
        .into_iter()
        .filter(|op| op.kind == OpKind::CreateExcessCheck)
        .collect();
    assert_eq!(excess.len(), 1);
    assert_eq!(excess[0].status, OpStatus::Succeeded);
    assert_eq!(held(&e, order.id).await, None);

    e.world.claim_check(e.client, &url).unwrap();
    assert_eq!(e.world.balance(e.client, XR, Asset::Usdt), dec!(4.70));
    assert_eq!(e.ledger("asset:xr:USDT").await, dec!(1010.30));
    assert_eq!(e.ledger("revenue:fee:USDT").await, dec!(-0.30));
    assert_eq!(e.ledger("liability:payable:USDT").await, dec!(0));
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn small_overpayment_is_kept(pool: PgPool) {
    let e = setup(pool).await;
    let inv = shop_invoice(&e, InvoiceSpec::crypto(CB, usdt(dec!(10))));
    let order = quoted(&e, &inv).await;
    pay_in(&e, &order, XR, dec!(10.50)).await;
    e.drain().await;

    let (o, d) = e.order(order.id).await;
    assert_eq!(o.state, OrderState::Completed);
    assert_eq!(
        d.refund_amount, None,
        "0,20 меньше минимального возврата 0,50"
    );
    assert!(
        e.ops(order.id)
            .await
            .iter()
            .all(|op| op.kind != OpKind::CreateExcessCheck)
    );
    assert_eq!(e.ledger("revenue:excess:USDT").await, dec!(-0.20));
    assert_eq!(e.ledger("liability:payable:USDT").await, dec!(0));
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn expired_quote_without_money_frees_the_reserve(pool: PgPool) {
    let e = setup(pool).await;
    let inv = shop_invoice(&e, InvoiceSpec::crypto(CB, usdt(dec!(10))));
    let order = quoted(&e, &inv).await;
    assert_eq!(e.engine.expire_quotes().await.unwrap(), 0, "ещё не истекла");
    expire_quote(&e, order.id).await;
    assert_eq!(e.engine.expire_quotes().await.unwrap(), 1);
    let (o, _) = e.order(order.id).await;
    assert_eq!(o.state, OrderState::Expired);
    assert_eq!(held(&e, order.id).await, None);

    // Чек, присланный после этого, — обычный обмен, а не оплата счёта.
    let url = e.world.client_check(XR, usdt(dec!(10.30))).unwrap();
    match e.submit(&url).await {
        Submitted::Accepted(o) => assert_ne!(o.id, order.id),
        other => panic!("{other:?}"),
    }
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn check_after_the_quote_expired_is_not_opened(pool: PgPool) {
    let e = setup(pool).await;
    let inv = shop_invoice(&e, InvoiceSpec::crypto(CB, usdt(dec!(10))));
    let order = quoted(&e, &inv).await;
    expire_quote(&e, order.id).await;
    let url = e.world.client_check(XR, usdt(dec!(10.30))).unwrap();
    assert_eq!(
        e.submit(&url).await,
        Submitted::Rejected {
            reason: SubmitReject::QuoteExpired,
            order: None
        }
    );
    assert!(
        e.ops(order.id)
            .await
            .iter()
            .all(|op| op.kind != OpKind::ActivateCheck)
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn expired_quote_with_part_paid_is_refunded(pool: PgPool) {
    let e = setup(pool).await;
    let inv = shop_invoice(&e, InvoiceSpec::crypto(CB, usdt(dec!(10))));
    let order = quoted(&e, &inv).await;
    pay_in(&e, &order, XR, dec!(5)).await;
    e.drain().await;
    expire_quote(&e, order.id).await;
    assert_eq!(e.engine.expire_quotes().await.unwrap(), 1);
    let (o, d) = e.order(order.id).await;
    assert_eq!(o.state, OrderState::RefundPending);
    assert_eq!(d.refund_amount, Some(dec!(5)));
    e.drain().await;

    let (o, d) = e.order(order.id).await;
    assert_eq!(o.state, OrderState::Refunded);
    e.world
        .claim_check(e.client, &d.payout_check_url.unwrap())
        .unwrap();
    assert_eq!(e.world.balance(e.client, XR, Asset::Usdt), dec!(5));
    assert_eq!(e.ledger("asset:xr:USDT").await, dec!(1000));
    assert_eq!(e.ledger("liability:payable:USDT").await, dec!(0));
    assert_eq!(
        e.world.invoice(&inv.code).unwrap().status,
        SimInvoiceStatus::Active,
        "счёт не тронут"
    );
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn client_cancels_a_quote(pool: PgPool) {
    let e = setup(pool).await;
    let inv = shop_invoice(&e, InvoiceSpec::crypto(CB, usdt(dec!(10))));
    let order = quoted(&e, &inv).await;
    assert!(
        !e.engine.cancel_invoice(999, order.id).await.unwrap(),
        "чужую заявку не отменить"
    );
    assert!(e.engine.cancel_invoice(CLIENT, order.id).await.unwrap());
    let (o, _) = e.order(order.id).await;
    assert_eq!(o.state, OrderState::Cancelled);
    assert_eq!(held(&e, order.id).await, None);
    assert!(
        !e.engine.cancel_invoice(CLIENT, order.id).await.unwrap(),
        "второй раз нечего"
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn invoice_that_expires_before_payment_is_refunded(pool: PgPool) {
    let e = setup(pool).await;
    let inv = shop_invoice(&e, InvoiceSpec::crypto(CB, usdt(dec!(10))));
    let order = quoted(&e, &inv).await;
    pay_in(&e, &order, XR, dec!(10.30)).await;
    // Продавец отменил счёт, пока шёл приём чека.
    e.world.expire_invoice(&inv.code).unwrap();
    e.drain().await;

    let (o, d) = e.order(order.id).await;
    assert_eq!(o.state, OrderState::Refunded);
    assert_eq!(d.refund_amount, Some(dec!(10.30)));
    assert_eq!(d.failure_code.as_deref(), Some("invoice_unpayable"));
    assert!(e.world.invoice(&inv.code).unwrap().payments.is_empty());
    e.world
        .claim_check(e.client, &d.payout_check_url.unwrap())
        .unwrap();
    assert_eq!(e.world.balance(e.client, XR, Asset::Usdt), dec!(10.30));
    assert_eq!(e.ledger("asset:cb:USDT").await, dec!(1000));
    assert_eq!(e.ledger("liability:payable:USDT").await, dec!(0));
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn unsuitable_invoices_are_refused_before_any_money(pool: PgPool) {
    let e = setup(pool).await;
    let reusable = shop_invoice(&e, InvoiceSpec::crypto(CB, usdt(dec!(10))).multi_use());
    assert_eq!(
        rejected(submit_invoice(&e, &reusable).await),
        InvoiceReject::Unsuitable(InvoiceIssue::Reusable)
    );
    let expired = shop_invoice(&e, InvoiceSpec::crypto(CB, usdt(dec!(10))));
    e.world.expire_invoice(&expired.code).unwrap();
    assert_eq!(
        rejected(submit_invoice(&e, &expired).await),
        InvoiceReject::Unsuitable(InvoiceIssue::Expired)
    );
    let ton = shop_invoice(
        &e,
        InvoiceSpec::crypto(CB, Money::new(dec!(3), Asset::Ton).unwrap()),
    );
    assert_eq!(
        rejected(submit_invoice(&e, &ton).await),
        InvoiceReject::Unsuitable(InvoiceIssue::UnsupportedAsset)
    );
    // Лимиты направления — по сумме, которую пришлёт клиент.
    let tiny = shop_invoice(&e, InvoiceSpec::crypto(CB, usdt(dec!(1))));
    assert_eq!(
        rejected(submit_invoice(&e, &tiny).await),
        InvoiceReject::BelowMinimum { min: dec!(2) }
    );
    let big = shop_invoice(&e, InvoiceSpec::crypto(CB, usdt(dec!(150))));
    assert_eq!(
        rejected(submit_invoice(&e, &big).await),
        InvoiceReject::AboveMaximum { max: dec!(100) },
        "лимит нового клиента"
    );
    let mut conn = e.pool.acquire().await.unwrap();
    let orders: i64 = sqlx::query_scalar("SELECT count(*) FROM orders")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(orders, 0, "отказы до денег заявок не создают");
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn one_invoice_is_paid_once(pool: PgPool) {
    let e = setup(pool).await;
    let inv = shop_invoice(&e, InvoiceSpec::crypto(CB, usdt(dec!(10))));
    let order = quoted(&e, &inv).await;
    assert_eq!(
        rejected(submit_invoice(&e, &inv).await),
        InvoiceReject::AlreadyInWork {
            order: order.public_id.clone()
        }
    );
    let other = shop_invoice(&e, InvoiceSpec::crypto(CB, usdt(dec!(5))));
    assert_eq!(
        rejected(submit_invoice(&e, &other).await),
        InvoiceReject::PendingInvoice {
            order: order.public_id.clone()
        },
        "сначала закончить или отменить первый счёт"
    );
    pay_in(&e, &order, XR, dec!(10.30)).await;
    e.drain().await;
    assert_eq!(
        rejected(submit_invoice(&e, &inv).await),
        InvoiceReject::AlreadyPaid {
            order: order.public_id.clone()
        }
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn cryptobot_invoice_without_the_pin_step_goes_to_a_human(pool: PgPool) {
    let e = env_without_payer(pool).await;
    open_invoices(&e).await;
    let inv = shop_invoice(&e, InvoiceSpec::crypto(CB, usdt(dec!(10))));
    let order = quoted(&e, &inv).await;
    pay_in(&e, &order, XR, dec!(10.30)).await;
    e.drain().await;

    let (o, d) = e.order(order.id).await;
    assert_eq!(o.state, OrderState::ManualReview);
    assert!(d.manual_reason.unwrap().contains("pay manually"));
    assert_eq!(
        e.world.invoice(&inv.code).unwrap().status,
        SimInvoiceStatus::Active
    );
    // Деньги клиента у нас (долг), резерв под счёт держится, пока оператор не решит.
    assert_eq!(e.ledger("liability:payable:USDT").await, dec!(-10.30));
    assert_eq!(held(&e, order.id).await, Some(dec!(10)));
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn unknown_payment_outcome_is_never_pressed_again(pool: PgPool) {
    let e = setup(pool).await;
    let inv = shop_invoice(&e, InvoiceSpec::crypto(XR, usdt(dec!(10))));
    let order = quoted(&e, &inv).await;
    pay_in(&e, &order, CB, dec!(10.20)).await;
    // Кнопка валюты — как обычно; «Оплатить» срабатывает, но бот молчит.
    e.world
        .inject(e.ub, Call::Press, Fault::DelayReply(Duration::ZERO), 1);
    e.world.inject(e.ub, Call::Press, Fault::NoReply, 1);
    e.drain().await;

    let (o, _) = e.order(order.id).await;
    assert_eq!(o.state, OrderState::ManualReview);
    let pay: Vec<_> = e
        .ops(order.id)
        .await
        .into_iter()
        .filter(|op| op.kind == OpKind::PayInvoice)
        .collect();
    assert_eq!(pay.len(), 1);
    assert_eq!(pay[0].status, OpStatus::Unknown);
    assert_eq!(e.world.invoice(&inv.code).unwrap().payments.len(), 1);

    // Рестарт и сверки не нажимают «Оплатить» второй раз.
    e.engine.recover().await.unwrap();
    e.drain().await;
    assert_eq!(e.world.invoice(&inv.code).unwrap().payments.len(), 1);
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn restart_in_the_middle_of_a_payment_never_pays_twice(pool: PgPool) {
    let e = setup(pool).await;
    let inv = shop_invoice(&e, InvoiceSpec::crypto(CB, usdt(dec!(10))));
    let order = quoted(&e, &inv).await;
    pay_in(&e, &order, XR, dec!(10.30)).await;
    // Активация чека клиента — до конца: движок, который берёт одну операцию за проход.
    let mut one = engine::Engine::new(
        e.pool.clone(),
        engine::EngineConfig {
            batch: 1,
            unavailable_backoff: Duration::ZERO,
            activation_retry_backoff: Duration::ZERO,
            reconcile_grace: Duration::ZERO,
            ..engine::EngineConfig::default()
        },
    );
    for w in e.wallets.values() {
        one = one.with_wallet(w.clone() as std::sync::Arc<dyn WalletBot>);
    }
    assert_eq!(one.process_due().await.unwrap(), 1);
    let (o, _) = e.order(order.id).await;
    assert_eq!(o.state, OrderState::InvoicePayPending);

    // Воркер взял оплату, юзербот оплатил счёт — и процесс упал до записи итога.
    let op = {
        let mut conn = e.pool.acquire().await.unwrap();
        storage::operations::claim_due(&mut conn, 1)
            .await
            .unwrap()
            .pop()
            .unwrap()
    };
    assert_eq!(op.kind, OpKind::PayInvoice);
    let tag = userbot::OpTag {
        key: op.idempotency_key.clone(),
        random_id: op.random_id,
    };
    let out = e.wallets[&CB]
        .pay_invoice(
            op.start_param.as_deref().unwrap(),
            Asset::Usdt,
            &usdt(dec!(10)),
            &tag,
        )
        .await
        .unwrap();
    assert_eq!(out, userbot::PayOutcome::Paid);

    e.engine.recover().await.unwrap();
    e.drain().await;
    let (o, _) = e.order(order.id).await;
    assert_eq!(
        o.state,
        OrderState::ManualReview,
        "исход неизвестен — человек"
    );
    assert_eq!(e.world.invoice(&inv.code).unwrap().payments.len(), 1);
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn excess_check_issued_with_a_timeout_is_found_not_repeated(pool: PgPool) {
    let e = setup(pool).await;
    let inv = shop_invoice(&e, InvoiceSpec::crypto(CB, usdt(dec!(10))));
    let order = quoted(&e, &inv).await;
    pay_in(&e, &order, XR, dec!(15)).await;
    // Единственный инлайн-чек в сценарии — чек на переплату: создан, а вызов вернул таймаут.
    e.world
        .inject(e.ub, Call::SendInline, Fault::TimeoutAfterEffect, 1);
    e.drain().await;
    e.engine.resolve_unknown().await.unwrap();

    let (o, d) = e.order(order.id).await;
    assert_eq!(o.state, OrderState::Completed);
    let excess: Vec<_> = e
        .ops(order.id)
        .await
        .into_iter()
        .filter(|op| op.kind == OpKind::CreateExcessCheck)
        .collect();
    assert_eq!(excess.len(), 1);
    assert_eq!(excess[0].status, OpStatus::Succeeded, "найден сверкой");
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1, "второго чека нет");
    let url = d.payout_check_url.expect("ссылка на чек переплаты");
    e.world.claim_check(e.client, &url).unwrap();
    assert_eq!(e.world.balance(e.client, XR, Asset::Usdt), dec!(4.70));
    assert_eq!(e.ledger("liability:payable:USDT").await, dec!(0));
    e.assert_books_match().await;
}
