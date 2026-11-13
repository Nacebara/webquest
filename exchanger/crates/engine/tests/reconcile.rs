//! Сверка балансов кошельков с леджером и самопроверка на кошельках (симулятор + PostgreSQL).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::{CB, Env, XR, accepted, env, usdt};
use domain::fsm::OrderState;
use domain::{Asset, Direction};
use engine::reconcile::Verdict;
use rust_decimal_macros::dec;
use sqlx::PgPool;
use storage::operations::{OpStatus, OperationRow};
use userbot::sim::{Call, Fault};

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

/// Баланс кошелька не читается — не тишина: событие владельцу с причиной; прочитался —
/// событие закрыто.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn unreadable_balance_raises_an_event_with_the_reason(pool: PgPool) {
    let e = env(pool).await;
    // Оба кошелька не отвечают на `/wallet` (один аккаунт на оба).
    e.world.inject(
        e.ub,
        userbot::sim::Call::SendText,
        userbot::sim::Fault::NoReply,
        2,
    );
    let r = e.engine.reconcile_balances().await.unwrap();
    assert!(r.iter().all(|x| x.verdict == Verdict::Unavailable), "{r:?}");
    assert!(
        r.iter()
            .all(|x| x.error.as_deref().is_some_and(|m| !m.is_empty()))
    );
    let mut conn = e.pool.acquire().await.unwrap();
    for p in [CB, XR] {
        let a = storage::alerts::open_by_key(&mut conn, &engine::reconcile::unreadable_key(p))
            .await
            .unwrap()
            .expect("событие «баланс не читается»");
        assert_eq!(a.title, "balance_unreadable");
        assert!(!a.payload["error"].as_str().unwrap_or("").is_empty());
    }
    drop(conn);

    e.world.clear_faults(e.ub);
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    let r = e.engine.reconcile_balances().await.unwrap();
    assert!(r.iter().all(|x| x.verdict == Verdict::Ok), "{r:?}");
    let mut conn = e.pool.acquire().await.unwrap();
    for p in [CB, XR] {
        assert!(
            storage::alerts::open_by_key(&mut conn, &engine::reconcile::unreadable_key(p))
                .await
                .unwrap()
                .is_none(),
            "прочитался — событие закрыто"
        );
    }
}

// ── Сверка при операции с неясным исходом (М-6, шаг 1.5) ─────────────────────────────────────

/// Обмен CryptoBot → xRocket на 100: чек выплаты (97.50) создан, но кошелёк ответил позже, чем
/// ждал сценарий, а история была недоступна — «выполнил и вернул таймаут». Выплата остаётся
/// `unknown`, деньги с xRocket уже ушли, леджер выплату ещё не провёл.
async fn unknown_payout(e: &Env) -> OperationRow {
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    let order = accepted(e.submit(&url).await);
    e.world.inject(
        e.ub,
        Call::CheckPress,
        Fault::DelayReply(Duration::from_millis(700)),
        1,
    );
    e.world
        .inject(e.ub, Call::History, Fault::DelayReply(Duration::ZERO), 2);
    e.world.inject(e.ub, Call::History, Fault::Disconnected, 3);
    e.drain().await;
    let (o, _) = e.order(order.id).await;
    assert_eq!(o.state, OrderState::PayoutPending);
    let payout = e.ops(order.id).await.pop().unwrap();
    assert_eq!(payout.status, OpStatus::Unknown);
    assert_eq!(payout.amount, Some(dec!(97.50)));
    tokio::time::sleep(Duration::from_millis(800)).await;
    e.world.clear_faults(e.ub);
    assert_eq!(e.world.balance(e.ub, XR, Asset::Usdt), dec!(902.50));
    payout
}

async fn row(e: &Env, platform: domain::Platform) -> (String, Vec<i64>, Option<Vec<i64>>) {
    let mut conn = e.pool.acquire().await.unwrap();
    let (wallet, _) = storage::wallets::active(&mut conn, platform)
        .await
        .unwrap()
        .unwrap();
    sqlx::query_as(
        "SELECT verdict, unsettled_ops, matched_ops FROM reconciliations WHERE wallet_account_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(wallet)
    .fetch_one(&mut *conn)
    .await
    .unwrap()
}

/// Чек выплаты на деле создан: баланс сходится с вариантом «выплата выполнена» — сверка не
/// молчит и не пугает, а записывает подсказку оператору: судя по балансу, выплата прошла.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn unknown_payout_that_went_through_matches_its_variant(pool: PgPool) {
    let e = env(pool).await;
    let payout = unknown_payout(&e).await;
    let r = e.engine.reconcile_balances().await.unwrap();
    let xr = r.iter().find(|x| x.platform == XR).unwrap();
    assert_eq!(xr.verdict, Verdict::Ok, "{r:?}");
    assert_eq!(xr.unsettled, vec![payout.id]);
    assert_eq!(xr.matched, Some(vec![payout.id]));
    assert_eq!(
        xr.expected,
        dec!(1000),
        "ожидание по леджеру — без неясной выплаты"
    );
    assert_eq!(
        row(&e, XR).await,
        ("ok".to_owned(), vec![payout.id], Some(vec![payout.id]))
    );
    let mut conn = e.pool.acquire().await.unwrap();
    assert!(
        storage::alerts::open(&mut conn, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

/// Аудит М-6: пока висит неясная выплата, с xRocket пропали деньги. Баланс не сходится ни с
/// одним вариантом — недостача ловится за один цикл сверки: CRITICAL и пауза выплат в xRocket.
/// Деньги нашлись (выплата не прошла) — порядок, подсказка «ничего не выполнено».
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn missing_money_is_caught_while_a_payout_is_unknown(pool: PgPool) {
    let e = env(pool).await;
    let payout = unknown_payout(&e).await;
    // Чек выплаты создан и ещё 12.00 ушли неизвестно куда.
    e.world
        .set_balance(e.ub, XR, Asset::Usdt, dec!(890.50))
        .unwrap();
    let r = e.engine.reconcile_balances().await.unwrap();
    let xr = r.iter().find(|x| x.platform == XR).unwrap();
    assert_eq!(xr.verdict, Verdict::Pause, "{r:?}");
    assert_eq!(xr.matched, None);
    let mut conn = e.pool.acquire().await.unwrap();
    for (d, paused) in [
        (Direction::CbToXrCheck, true),
        (Direction::PayXrInvoice, true),
        (Direction::XrToCbCheck, false),
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
    let a = storage::alerts::open_by_key(&mut conn, &engine::reconcile::mismatch_key(XR))
        .await
        .unwrap()
        .expect("тревога о недостаче");
    assert_eq!(a.level, storage::alerts::Level::Critical);
    assert_eq!(
        a.payload["expected"], "902.5",
        "сравнение — с вариантом «выплата прошла»"
    );
    assert_eq!(a.payload["ledger"], "1000");
    assert_eq!(a.payload["unsettled"], serde_json::json!([payout.id]));
    drop(conn);

    // Выплата на деле не прошла, денег столько же, сколько по леджеру.
    e.world
        .set_balance(e.ub, XR, Asset::Usdt, dec!(1000))
        .unwrap();
    let r = e.engine.reconcile_balances().await.unwrap();
    let xr = r.iter().find(|x| x.platform == XR).unwrap();
    assert_eq!(
        (xr.verdict, xr.matched.clone()),
        (Verdict::Ok, Some(vec![]))
    );
    let mut conn = e.pool.acquire().await.unwrap();
    assert!(
        storage::alerts::open_by_key(&mut conn, &engine::reconcile::mismatch_key(XR))
            .await
            .unwrap()
            .is_none()
    );
}

/// Операция прямо сейчас в работе (`dispatched`) — деньги в пути, замер не делается, как раньше.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn an_operation_in_progress_still_skips_the_wallet(pool: PgPool) {
    let e = env(pool).await;
    let payout = unknown_payout(&e).await;
    sqlx::query("UPDATE operations SET status = 'dispatched' WHERE id = $1")
        .bind(payout.id)
        .execute(&e.pool)
        .await
        .unwrap();
    let r = e.engine.reconcile_balances().await.unwrap();
    let xr = r.iter().find(|x| x.platform == XR).unwrap();
    assert_eq!(xr.verdict, Verdict::SkippedInflight);
    assert_eq!(row(&e, XR).await.0, "skipped_inflight");
}

/// Сверка не проходит дольше порога (по умолчанию 30 минут) — CRITICAL «сверка не проходит» с
/// операциями, которые её держат; прошла — тревога закрыта сама.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_stalled_reconciliation_raises_an_alert_and_clears(pool: PgPool) {
    let e = env(pool).await;
    let payout = unknown_payout(&e).await;
    sqlx::query("UPDATE operations SET status = 'dispatched' WHERE id = $1")
        .bind(payout.id)
        .execute(&e.pool)
        .await
        .unwrap();
    e.engine.reconcile_balances().await.unwrap();
    let key = engine::reconcile::stalled_key(XR);
    let mut conn = e.pool.acquire().await.unwrap();
    assert!(
        storage::alerts::open_by_key(&mut conn, &key)
            .await
            .unwrap()
            .is_none(),
        "один пропуск — ещё не беда"
    );
    sqlx::query("UPDATE reconciliations SET at = now() - interval '40 minutes' WHERE verdict = 'skipped_inflight'")
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);
    e.engine.reconcile_balances().await.unwrap();
    let mut conn = e.pool.acquire().await.unwrap();
    let a = storage::alerts::open_by_key(&mut conn, &key)
        .await
        .unwrap()
        .expect("тревога «сверка не проходит»");
    assert_eq!(a.title, "reconcile_stalled");
    assert_eq!(a.level, storage::alerts::Level::Critical);
    assert!(a.payload["minutes"].as_i64().unwrap() >= 40);
    assert_eq!(a.payload["ops"][0]["id"], payout.id);
    assert_eq!(a.payload["ops"][0]["order_id"], payout.order_id.unwrap());
    // Операция снова «неясная» с известной суммой — сверка проходит по вариантам.
    sqlx::query("UPDATE operations SET status = 'unknown' WHERE id = $1")
        .bind(payout.id)
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);
    let r = e.engine.reconcile_balances().await.unwrap();
    assert!(r.iter().all(|x| x.verdict == Verdict::Ok), "{r:?}");
    let mut conn = e.pool.acquire().await.unwrap();
    assert!(
        storage::alerts::open_by_key(&mut conn, &key)
            .await
            .unwrap()
            .is_none()
    );
}
