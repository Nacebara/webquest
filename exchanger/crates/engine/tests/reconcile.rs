//! Сверка балансов кошельков с леджером и самопроверка на кошельках (симулятор + PostgreSQL).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{CB, XR, env};
use domain::{Asset, Direction};
use engine::reconcile::Verdict;
use rust_decimal_macros::dec;
use sqlx::PgPool;

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn books_that_match_are_ok_and_quiet(pool: PgPool) {
    let e = env(pool).await;
    let r = e.engine.reconcile_balances().await.unwrap();
    assert_eq!(r.len(), 2);
    assert!(
        r.iter()
            .all(|x| x.verdict == Verdict::Ok && x.diff() == Some(dec!(0)))
    );
    let mut conn = e.pool.acquire().await.unwrap();
    assert!(
        storage::alerts::open(&mut conn, 10)
            .await
            .unwrap()
            .is_empty()
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM reconciliations WHERE verdict = 'ok'")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(n, 2);
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn owner_top_up_is_a_warning_with_the_difference_not_a_pause(pool: PgPool) {
    let e = env(pool).await;
    e.world
        .set_balance(e.ub, CB, Asset::Usdt, dec!(1100))
        .unwrap();
    let r = e.engine.reconcile_balances().await.unwrap();
    let cb = r.iter().find(|x| x.platform == CB).unwrap();
    assert_eq!(cb.verdict, Verdict::Warn);
    assert_eq!(cb.diff(), Some(dec!(100)));
    let mut conn = e.pool.acquire().await.unwrap();
    let alerts = storage::alerts::open(&mut conn, 10).await.unwrap();
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].dedup_key, "balance_mismatch:cryptobot");
    assert_eq!(alerts[0].payload["diff"], "100");
    for d in Direction::ALL {
        assert!(
            !storage::directions::get(&mut conn, d)
                .await
                .unwrap()
                .auto_paused
        );
    }
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn missing_money_pauses_payouts_from_that_wallet(pool: PgPool) {
    let e = env(pool).await;
    e.world
        .set_balance(e.ub, CB, Asset::Usdt, dec!(950))
        .unwrap();
    let r = e.engine.reconcile_balances().await.unwrap();
    let cb = r.iter().find(|x| x.platform == CB).unwrap();
    assert_eq!(cb.verdict, Verdict::Pause);
    let mut conn = e.pool.acquire().await.unwrap();
    // Выплаты с CryptoBot — встречный обмен и оплата счетов CryptoBot.
    for (d, paused) in [
        (Direction::XrToCbCheck, true),
        (Direction::PayCbInvoice, true),
        (Direction::CbToXrCheck, false),
        (Direction::PayXrInvoice, false),
    ] {
        assert_eq!(
            storage::directions::get(&mut conn, d)
                .await
                .unwrap()
                .auto_paused,
            paused,
            "{d:?}"
        );
    }
    let alerts = storage::alerts::open(&mut conn, 10).await.unwrap();
    assert_eq!(alerts[0].level, storage::alerts::Level::Critical);
    // Деньги вернулись — событие закрывается само.
    drop(conn);
    e.world
        .set_balance(e.ub, CB, Asset::Usdt, dec!(1000))
        .unwrap();
    e.engine.reconcile_balances().await.unwrap();
    let mut conn = e.pool.acquire().await.unwrap();
    assert!(
        storage::alerts::open(&mut conn, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn selftest_issues_and_claims_a_check_on_each_wallet_and_moves_no_money(pool: PgPool) {
    let e = env(pool).await;
    let steps = e.engine.selftest(dec!(1)).await.unwrap();
    assert!(steps.iter().all(|s| s.ok), "{steps:#?}");
    for platform in [CB, XR] {
        for step in ["balance", "issue", "claim", "balance_after"] {
            assert!(
                steps
                    .iter()
                    .any(|s| s.platform == platform && s.step == step),
                "{platform:?} {step}"
            );
        }
        assert_eq!(e.world.balance(e.ub, platform, Asset::Usdt), dec!(1000));
    }
    assert_eq!(e.world.checks_created_by(e.ub).len(), 2);
    e.assert_books_match().await;
    // После самопроверки сверка снова работает.
    let r = e.engine.reconcile_balances().await.unwrap();
    assert!(r.iter().all(|x| x.verdict == Verdict::Ok));
}
