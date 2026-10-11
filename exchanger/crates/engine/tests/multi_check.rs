//! Мульти-чеки (решение владельца, DESIGN-v0.2 Р8) и повторная ссылка на чек, который мы уже
//! открывали:
//! - мульти-чек CryptoBot (`CQ…`) — как обычный чек: зачисляется наша доля из ответа бота;
//! - мульти-чек xRocket (`mci_…`, `mc_…`) активируется только в приложении xRocket — `/start`
//!   такого чека боту не уходит никогда;
//! - тот же чек второй раз (другой клиент прислал ту же ссылку на мульти-чек из канала, или
//!   клиент повторил старый чек) — отказ без второго `/start`, а не внутренняя ошибка.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{CB, CLIENT, XR, accepted, env, usdt};
use domain::fsm::{OpKind, OrderState};
use domain::{Asset, Direction};
use engine::{InvoiceSubmitted, SubmitReject, Submitted};
use parsers::{LinkKind, WalletLink};
use rust_decimal_macros::dec;
use sqlx::PgPool;
use userbot::Chat;
use userbot::sim::{CheckSpec, CheckStatus, InvoiceSpec};

async fn count(pool: &PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

/// Все `/start`, которые юзербот отправил боту платформы.
fn starts(e: &common::Env, platform: domain::Platform) -> Vec<String> {
    e.world
        .messages(e.ub, Chat::WalletBot(platform))
        .into_iter()
        .filter(|m| m.out && m.text.starts_with("/start"))
        .map(|m| m.text)
        .collect()
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn cryptobot_multi_check_is_exchanged_like_a_regular_check(pool: PgPool) {
    let e = env(pool).await;
    // Мульти-чек на три активации по 10 USDT.
    let multi = e
        .world
        .create_check(CheckSpec::new(CB, usdt(dec!(10))).multi(3))
        .unwrap();
    assert!(multi.code.starts_with("CQ"), "у CryptoBot тот же формат");
    let order = accepted(e.submit(&multi.url).await);
    e.drain().await;

    let (order, d) = e.order(order.id).await;
    assert_eq!(order.state, OrderState::Completed);
    assert_eq!(
        d.intake_amount,
        Some(dec!(10)),
        "наша доля — из ответа бота"
    );
    assert_eq!(d.payout_amount, Some(dec!(9.75)));
    let check = e.world.check(&multi.code).unwrap();
    assert_eq!(check.claimed_by, vec![e.ub], "одна активация — наша");
    assert_eq!(check.status, CheckStatus::Active, "остальные доли — в чеке");
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn the_same_multi_check_from_another_client_is_refused_without_a_second_start(pool: PgPool) {
    let e = env(pool).await;
    let multi = e
        .world
        .create_check(CheckSpec::new(CB, usdt(dec!(10))).multi(3))
        .unwrap();
    accepted(e.submit(&multi.url).await);
    e.drain().await;
    let starts_before = starts(&e, CB);
    assert_eq!(starts_before.len(), 1);

    // Ту же ссылку (из канала) присылает другой клиент.
    let other = CLIENT + 1;
    {
        let mut conn = e.pool.acquire().await.unwrap();
        storage::users::upsert(&mut conn, other, &storage::users::Profile::default())
            .await
            .unwrap();
    }
    let link = parsers::links::parse_url(&multi.url).unwrap();
    assert_eq!(
        e.engine.submit_check(other, &link).await.unwrap(),
        Submitted::Rejected {
            reason: SubmitReject::CheckUsed,
            order: None
        }
    );
    // И тот же клиент — повторно.
    assert_eq!(
        e.submit(&multi.url).await,
        Submitted::Rejected {
            reason: SubmitReject::CheckUsed,
            order: None
        }
    );
    e.drain().await;
    assert_eq!(starts(&e, CB), starts_before, "второго /start нет");
    assert_eq!(count(&e.pool, "SELECT count(*) FROM orders").await, 1);
    assert_eq!(
        count(
            &e.pool,
            "SELECT count(*) FROM operations WHERE kind = 'activate_check'"
        )
        .await,
        1
    );
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_regular_check_sent_again_after_the_exchange_is_refused(pool: PgPool) {
    let e = env(pool).await;
    let url = e.world.client_check(XR, usdt(dec!(20))).unwrap();
    accepted(e.submit(&url).await);
    e.drain().await;
    assert_eq!(
        e.submit(&url).await,
        Submitted::Rejected {
            reason: SubmitReject::CheckUsed,
            order: None
        }
    );
    assert_eq!(starts(&e, XR).len(), 1);
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_check_whose_activation_failed_can_be_sent_again(pool: PgPool) {
    let e = env(pool).await;
    // Чек ещё не создан (клиент прислал ссылку раньше времени) — «не найден», заявка закрыта.
    let url = "https://t.me/send?start=CQAbCdEfGhIj";
    let order = accepted(e.submit(url).await);
    e.drain().await;
    let (order, _) = e.order(order.id).await;
    assert_eq!(order.state, OrderState::IntakeFailed);
    // Неуспешная активация чек не «занимает»: его можно прислать снова.
    assert!(matches!(e.submit(url).await, Submitted::Accepted(_)));
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn xrocket_multi_check_never_reaches_the_bot(pool: PgPool) {
    let e = env(pool).await;
    let multi = e
        .world
        .create_check(CheckSpec::new(XR, usdt(dec!(5))).multi(2))
        .unwrap();
    assert!(multi.code.starts_with("mci_"));
    // Парсер такую ссылку чеком не отдаёт.
    assert_eq!(
        parsers::links::parse_url(&multi.url),
        Err(parsers::LinkError::MultiCheck)
    );
    // Даже если ссылка как-то дошла до движка чеком — отказ, `/start` не уходит.
    for param in [multi.code.as_str(), "mc_VhxQyBOeQXDmHAX", "mc_ABCDEFGHIJ"] {
        let link = WalletLink {
            platform: domain::Platform::XRocket,
            kind: LinkKind::Check,
            param: param.to_owned(),
        };
        assert_eq!(
            e.engine.submit_check(CLIENT, &link).await.unwrap(),
            Submitted::Rejected {
                reason: SubmitReject::MultiCheck,
                order: None
            },
            "{param}"
        );
    }
    e.drain().await;
    assert!(starts(&e, XR).is_empty());
    assert_eq!(count(&e.pool, "SELECT count(*) FROM orders").await, 0);
    assert!(e.world.check(&multi.code).unwrap().claimed_by.is_empty());
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_used_check_does_not_fund_an_invoice(pool: PgPool) {
    let e = env(pool).await;
    {
        let mut conn = e.pool.acquire().await.unwrap();
        storage::directions::set_open(&mut conn, Direction::PayCbInvoice, true)
            .await
            .unwrap();
    }
    // Чек уже обменян.
    let url = e.world.client_check(XR, usdt(dec!(10.30))).unwrap();
    accepted(e.submit(&url).await);
    e.drain().await;
    // Клиент берёт счёт и присылает тот же чек.
    let shop = e.world.add_account("shop");
    let inv = e
        .world
        .create_invoice(InvoiceSpec::crypto(CB, usdt(dec!(10))).by(shop))
        .unwrap();
    let link = parsers::links::parse_url(&inv.url).unwrap();
    let order = match e.engine.submit_invoice(CLIENT, &link).await.unwrap() {
        InvoiceSubmitted::Quoted(order) => order,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        e.submit(&url).await,
        Submitted::Rejected {
            reason: SubmitReject::CheckUsed,
            order: None
        }
    );
    let (o, _) = e.order(order.id).await;
    assert_eq!(o.state, OrderState::AwaitingFunds, "счёт ждёт другой чек");
    assert!(
        e.ops(order.id)
            .await
            .iter()
            .all(|op| op.kind != OpKind::ActivateCheck)
    );
    assert_eq!(e.world.balance(e.ub, XR, Asset::Usdt), dec!(1010.30));
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn an_earlier_check_of_a_live_invoice_is_not_opened_again(pool: PgPool) {
    let e = env(pool).await;
    {
        let mut conn = e.pool.acquire().await.unwrap();
        storage::directions::set_open(&mut conn, Direction::PayCbInvoice, true)
            .await
            .unwrap();
    }
    let shop = e.world.add_account("shop");
    let inv = e
        .world
        .create_invoice(InvoiceSpec::crypto(CB, usdt(dec!(10))).by(shop))
        .unwrap();
    let link = parsers::links::parse_url(&inv.url).unwrap();
    let order = match e.engine.submit_invoice(CLIENT, &link).await.unwrap() {
        InvoiceSubmitted::Quoted(order) => order,
        other => panic!("{other:?}"),
    };
    // Два чека по 4 USDT: заявка всё ещё ждёт (нужно 10,30), в ней уже второй чек.
    let first = e.world.client_check(XR, usdt(dec!(4))).unwrap();
    let second = e.world.client_check(XR, usdt(dec!(4))).unwrap();
    for url in [&first, &second] {
        assert!(matches!(e.submit(url).await, Submitted::Accepted(_)));
        e.drain().await;
    }
    let (o, d) = e.order(order.id).await;
    assert_eq!(o.state, OrderState::AwaitingFunds);
    assert_eq!(d.intake_amount, Some(dec!(8)));
    // Первый чек ещё раз: в колонке заявки его уже нет, но мы его открывали.
    assert_eq!(
        e.submit(&first).await,
        Submitted::Rejected {
            reason: SubmitReject::CheckUsed,
            order: None
        }
    );
    assert_eq!(starts(&e, XR).len(), 2, "третьего /start нет");
    e.assert_books_match().await;
}
