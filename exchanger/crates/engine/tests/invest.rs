//! Инвесторы (DESIGN-v0.2 Р18) на симуляторе и настоящем PostgreSQL: пополнение чеком, вывод
//! чеком (сам и через владельца), сбой «чек создан, ответ потерян», начисление прибыли.
//! В каждом сценарии с деньгами: ≤ 1 чек на вывод, балансы симулятора = леджер.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{CB, CLIENT, XR, accepted, env, usdt};
use domain::fsm::OpKind;
use domain::{Decimal, Platform};
use engine::invest::{InvestError, InvestRefusal, InvestVerdict, investor_share};
use rust_decimal_macros::dec;
use sqlx::PgPool;
use storage::invest::{Investor, MoveStatus};
use storage::staff::StaffMember;

const INVESTOR: i64 = 222;

async fn owner(e: &common::Env) -> StaffMember {
    let mut conn = e.pool.acquire().await.unwrap();
    storage::staff::ensure_owner(&mut conn, 1, "Owner")
        .await
        .unwrap()
}

/// Инвестор с долей 50 %, без блокировки, автовывод до `auto`.
async fn grant(e: &common::Env, lock_days: i32, auto: Decimal) -> StaffMember {
    let staff = owner(e).await;
    {
        let mut conn = e.pool.acquire().await.unwrap();
        storage::users::upsert(&mut conn, INVESTOR, &storage::users::Profile::default())
            .await
            .unwrap();
    }
    e.engine
        .invest_save_terms(
            &staff,
            &Investor {
                user_id: INVESTOR,
                share_pct: dec!(50),
                lock_days,
                min_deposit: dec!(10),
                auto_withdraw_max: auto,
                is_active: true,
                granted_by: None,
                created_at: chrono::Utc::now(),
            },
        )
        .await
        .unwrap();
    staff
}

async fn deposit(e: &common::Env, platform: Platform, amount: Decimal) -> i64 {
    let url = e.world.client_check(platform, usdt(amount)).unwrap();
    let link = parsers::links::parse_url(&url).unwrap();
    let id = e.engine.invest_deposit(INVESTOR, &link).await.unwrap();
    e.drain().await;
    id
}

async fn balance(e: &common::Env) -> Decimal {
    -e.ledger(&format!("liability:investor:{INVESTOR}:USDT"))
        .await
}

async fn get_move(e: &common::Env, id: i64) -> storage::invest::Move {
    let mut conn = e.pool.acquire().await.unwrap();
    storage::invest::get(&mut conn, id, false).await.unwrap()
}

async fn active_holds(e: &common::Env) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM holds WHERE state = 'active'")
        .fetch_one(&e.pool)
        .await
        .unwrap()
}

fn refusal(r: Result<impl std::fmt::Debug, InvestError>) -> InvestRefusal {
    match r {
        Err(InvestError::Refused(r)) => r,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn deposit_is_credited_and_small_withdrawal_pays_itself(pool: PgPool) {
    let e = env(pool).await;
    grant(&e, 0, dec!(100)).await;

    let dep = deposit(&e, CB, dec!(200)).await;
    let m = get_move(&e, dep).await;
    assert_eq!(m.status, MoveStatus::Done);
    assert_eq!(m.amount, Some(dec!(200)));
    assert_eq!(balance(&e).await, dec!(200));
    e.assert_books_match().await;

    let w = e
        .engine
        .invest_withdraw(INVESTOR, XR, dec!(50))
        .await
        .unwrap();
    assert_eq!(
        w.status,
        MoveStatus::Processing,
        "в пределах автолимита — сразу"
    );
    e.drain().await;
    let w = get_move(&e, w.id).await;
    assert_eq!(w.status, MoveStatus::Done);
    let checks = e.world.checks_created_by(e.ub);
    assert_eq!(checks.len(), 1, "один чек на вывод");
    assert_eq!(w.check_url.as_deref(), Some(checks[0].url.as_str()));
    assert_eq!(balance(&e).await, dec!(150));
    assert_eq!(active_holds(&e).await, 0);
    e.world.claim_check(e.client, &checks[0].url).unwrap();
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn large_withdrawal_waits_for_the_owner(pool: PgPool) {
    let e = env(pool).await;
    let staff = grant(&e, 0, dec!(10)).await;
    deposit(&e, XR, dec!(300)).await;

    let w = e
        .engine
        .invest_withdraw(INVESTOR, CB, dec!(120))
        .await
        .unwrap();
    assert_eq!(w.status, MoveStatus::Requested);
    e.drain().await;
    assert!(
        e.world.checks_created_by(e.ub).is_empty(),
        "без решения — ничего"
    );
    // Вторая заявка, пока первая открыта, — нельзя.
    assert_eq!(
        refusal(e.engine.invest_withdraw(INVESTOR, CB, dec!(5)).await),
        InvestRefusal::OpenWithdrawal
    );
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM alerts WHERE title = 'invest_withdrawal' AND acked_at IS NULL",
    )
    .fetch_one(&e.pool)
    .await
    .unwrap();
    assert_eq!(open, 1, "владелец получил заявку");

    // Отклонил — деньги в обороте, можно просить снова.
    let r = e
        .engine
        .invest_reject(w.id, &staff, "рано, подожди неделю")
        .await
        .unwrap();
    assert_eq!(r.status, MoveStatus::Rejected);
    assert_eq!(balance(&e).await, dec!(300));
    assert_eq!(
        refusal(e.engine.invest_withdraw(INVESTOR, CB, dec!(301)).await),
        InvestRefusal::OverWithdrawable { max: dec!(300) }
    );

    let w = e
        .engine
        .invest_withdraw(INVESTOR, CB, dec!(120))
        .await
        .unwrap();
    let a = e.engine.invest_approve(w.id, &staff).await.unwrap();
    assert_eq!(a.status, MoveStatus::Processing);
    assert_eq!(
        refusal(e.engine.invest_approve(w.id, &staff).await),
        InvestRefusal::WrongState,
        "второе одобрение не создаст второй чек"
    );
    // Больше, чем есть, — нельзя (проверено выше), одобренная заявка закрыла событие.
    e.drain().await;
    assert_eq!(get_move(&e, w.id).await.status, MoveStatus::Done);
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1);
    assert_eq!(balance(&e).await, dec!(180));
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn fresh_deposits_are_locked(pool: PgPool) {
    let e = env(pool).await;
    grant(&e, 14, dec!(1000)).await;
    deposit(&e, CB, dec!(100)).await;
    assert_eq!(
        refusal(e.engine.invest_withdraw(INVESTOR, XR, dec!(1)).await),
        InvestRefusal::OverWithdrawable { max: dec!(0) }
    );
    // Тот же чек второй раз — отказ, деньги не тронуты.
    let used = e.world.checks()[0].url.clone();
    let link = parsers::links::parse_url(&used).unwrap();
    assert_eq!(
        refusal(e.engine.invest_deposit(INVESTOR, &link).await),
        InvestRefusal::CheckUsed
    );
    // Не инвестор — отказ.
    let url = e.world.client_check(CB, usdt(dec!(10))).unwrap();
    let link = parsers::links::parse_url(&url).unwrap();
    assert_eq!(
        refusal(e.engine.invest_deposit(CLIENT, &link).await),
        InvestRefusal::NotInvestor
    );
}

/// Чек вывода создан, а процесс упал до записи итога: после рестарта — сверка, ровно один
/// чек, проводка одна; повторного создания нет.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn crash_after_issuing_is_reconciled_to_one_check(pool: PgPool) {
    let e = env(pool).await;
    grant(&e, 0, dec!(100)).await;
    deposit(&e, CB, dec!(100)).await;
    let w = e
        .engine
        .invest_withdraw(INVESTOR, CB, dec!(40))
        .await
        .unwrap();

    let op = {
        let mut conn = e.pool.acquire().await.unwrap();
        storage::operations::claim_due(&mut conn, 1)
            .await
            .unwrap()
            .pop()
            .unwrap()
    };
    assert_eq!(op.kind, OpKind::CreatePayoutCheck);
    assert_eq!(op.invest_move_id, Some(w.id));
    let tag = userbot::OpTag {
        key: op.idempotency_key.clone(),
        random_id: op.random_id,
    };
    use userbot::WalletBot;
    let _ = e.wallets[&CB].issue_check(&usdt(dec!(40)), &tag).await;

    e.engine.recover().await.unwrap();
    e.drain().await;
    let w = get_move(&e, w.id).await;
    assert_eq!(w.status, MoveStatus::Done);
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1, "второго чека нет");
    assert_eq!(balance(&e).await, dec!(60));
    assert_eq!(active_holds(&e).await, 0);
    e.assert_books_match().await;
}

/// Неясный исход, чек не найден: к владельцу, удержание держит деньги, повтора нет. Владелец
/// проверил — чека нет: деньги снова у инвестора.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn unknown_withdrawal_goes_to_the_owner_and_never_repeats(pool: PgPool) {
    let e = env(pool).await;
    let staff = grant(&e, 0, dec!(100)).await;
    deposit(&e, XR, dec!(100)).await;
    let w = e
        .engine
        .invest_withdraw(INVESTOR, XR, dec!(30))
        .await
        .unwrap();
    // Процесс взял операцию и упал, не сделав ничего.
    {
        let mut conn = e.pool.acquire().await.unwrap();
        storage::operations::claim_due(&mut conn, 1).await.unwrap();
    }
    e.engine.recover().await.unwrap();
    e.drain().await;
    let m = get_move(&e, w.id).await;
    assert_eq!(m.status, MoveStatus::Manual);
    assert!(
        e.world.checks_created_by(e.ub).is_empty(),
        "чек не создаётся повторно"
    );
    assert_eq!(active_holds(&e).await, 1, "деньги удержаны до решения");
    e.engine.resolve_unknown().await.unwrap();
    e.drain().await;
    assert!(e.world.checks_created_by(e.ub).is_empty());

    let r = e
        .engine
        .invest_resolve(
            w.id,
            &staff,
            &InvestVerdict::DidNotHappen,
            "проверил кошелёк, чека нет",
        )
        .await
        .unwrap();
    assert_eq!(r.status, MoveStatus::Failed);
    assert_eq!(active_holds(&e).await, 0);
    assert_eq!(balance(&e).await, dec!(100));
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn profit_is_a_share_of_fee_revenue_by_capital(pool: PgPool) {
    let e = env(pool).await;
    grant(&e, 0, dec!(0)).await;
    deposit(&e, CB, dec!(1000)).await;
    // Окно до пополнения: выручки нет — ничего, курсор сдвинулся.
    let first = e.engine.invest_accrue().await.unwrap();
    assert!(first.credited.is_empty());

    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    accepted(e.submit(&url).await);
    e.drain().await;
    let revenue = -e.ledger("revenue:fee:USDT").await;
    assert!(revenue > Decimal::ZERO);

    let acc = e.engine.invest_accrue().await.unwrap();
    assert_eq!(acc.revenue, revenue);
    // Капитал на начало окна: 2 × 1000 владельца + 1000 инвестора.
    assert_eq!(acc.capital, dec!(3000));
    let expected = investor_share(revenue, dec!(1000), dec!(3000), dec!(50));
    assert!(expected > Decimal::ZERO);
    assert_eq!(acc.credited, vec![(INVESTOR, expected)]);
    assert_eq!(balance(&e).await, dec!(1000) + expected);

    // Новых проводок нет — второй раз не начислит.
    assert!(e.engine.invest_accrue().await.unwrap().credited.is_empty());
    assert_eq!(balance(&e).await, dec!(1000) + expected);
    e.assert_books_match().await;
}
