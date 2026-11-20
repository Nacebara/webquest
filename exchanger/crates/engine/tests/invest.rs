//! Инвесторы (DESIGN-v0.2 Р18) на симуляторе и настоящем PostgreSQL: пополнение чеком, вывод
//! чеком (сам и через владельца), сбой «чек создан, ответ потерян», начисление прибыли.
//! В каждом сценарии с деньгами: ≤ 1 чек на вывод, балансы симулятора = леджер.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::{CB, CLIENT, XR, accepted, env, usdt};
use domain::fsm::OpKind;
use domain::{Asset, Decimal, Platform};
use engine::invest::{InvestError, InvestRefusal, InvestVerdict, investor_share};
use rust_decimal_macros::dec;
use sqlx::PgPool;
use storage::invest::{Investor, MoveStatus};
use storage::staff::StaffMember;
use userbot::sim::{Call, Fault};

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

/// Д-19.5 (проверка 19.11): доход, проведённый, пока идёт начисление, не теряется. Пульт провёл
/// доход: номер проводки выдан, транзакция ещё не закоммичена; следом закоммичена проводка с
/// бо́льшим номером. Раньше начисление брало последний номер и сдвигало окно за неудачную проводку —
/// её доход не попадал ни в одно окно.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn revenue_posted_during_the_accrual_is_not_skipped(pool: PgPool) {
    let e = env(pool).await;
    grant(&e, 0, dec!(0)).await;
    deposit(&e, CB, dec!(1000)).await;
    e.engine.invest_accrue().await.unwrap();

    let mut slow = e.pool.begin().await.unwrap();
    for posting in [
        domain::ledger::intake(CB, Asset::Usdt, dec!(10)).unwrap(),
        domain::ledger::excess_kept(Asset::Usdt, dec!(10)).unwrap(),
    ] {
        storage::ledger::post(&mut slow, &posting, storage::ledger::PostingRefs::default())
            .await
            .unwrap();
    }
    let later = async {
        let mut tx = e.pool.begin().await.unwrap();
        storage::ledger::post(
            &mut tx,
            &domain::ledger::capital_in(XR, Asset::Usdt, dec!(1)).unwrap(),
            storage::ledger::PostingRefs {
                memo: Some("взнос владельца"),
                created_by: Some("staff:1"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    };
    // Проводка с бо́льшим номером закоммичена раньше (или ждёт начисления — тоже верно).
    let _ = tokio::time::timeout(Duration::from_millis(300), later).await;
    let commit_slow = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        slow.commit().await.unwrap();
    };
    let (first, ()) = tokio::join!(e.engine.invest_accrue(), commit_slow);
    let second = e.engine.invest_accrue().await.unwrap();
    assert_eq!(
        first.unwrap().revenue + second.revenue,
        dec!(10),
        "доход незакоммиченной проводки вошёл в окно"
    );
}

/// Шаг 5.2: пополнение инвестора, ответ на активацию потерян («выполнил и вернул таймаут» или
/// разрыв после отправки) — повтор активации видит, что чек уже наш, и зачисляет ровно один раз.
async fn deposit_with_a_lost_reply(pool: PgPool, fault: Fault) {
    let e = env(pool).await;
    grant(&e, 0, dec!(100)).await;
    let url = e.world.client_check(CB, usdt(dec!(150))).unwrap();
    let link = parsers::links::parse_url(&url).unwrap();
    e.world.inject(e.ub, Call::SendText, fault.clone(), 1);
    let id = e.engine.invest_deposit(INVESTOR, &link).await.unwrap();
    e.drain().await;
    e.engine.recover().await.unwrap();
    e.drain().await;
    let m = get_move(&e, id).await;
    assert_eq!(m.status, MoveStatus::Done, "{fault:?}");
    assert_eq!(balance(&e).await, dec!(150), "{fault:?}: одно зачисление");
    let credits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ledger_transactions t JOIN operations op ON op.id = t.operation_id \
          WHERE t.kind = 'invest_deposit' AND op.invest_move_id = $1",
    )
    .bind(id)
    .fetch_one(&e.pool)
    .await
    .unwrap();
    assert_eq!(credits, 1, "{fault:?}");
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_deposit_whose_reply_timed_out_is_credited_once(pool: PgPool) {
    deposit_with_a_lost_reply(pool, Fault::TimeoutAfterEffect).await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_deposit_whose_connection_dropped_is_credited_once(pool: PgPool) {
    deposit_with_a_lost_reply(pool, Fault::DisconnectedAfterEffect).await;
}

/// Шаг 5.4 (М-5, как 1.6 у заявки): пополнение на 3 USDT без ответа кошелька, владелец решает
/// «деньги пришли» — 300 вместо 3 движок не зачисляет: столько в кошельке сверх учёта нет. Замер
/// делает само решение (неотвеченная активация держала чат кошелька).
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_deposit_confirmed_by_hand_cannot_exceed_the_wallet(pool: PgPool) {
    let e = env(pool).await;
    let staff = grant(&e, 0, dec!(100)).await;
    let url = e.world.client_check(XR, usdt(dec!(3))).unwrap();
    let link = parsers::links::parse_url(&url).unwrap();
    e.world.inject(e.ub, Call::SendText, Fault::NoReply, 1);
    let id = e.engine.invest_deposit(INVESTOR, &link).await.unwrap();
    e.drain().await;
    assert_eq!(get_move(&e, id).await.status, MoveStatus::Manual);
    let r = e
        .engine
        .invest_resolve(
            id,
            &staff,
            &InvestVerdict::Happened {
                amount: Some(dec!(300)),
            },
            "чек нашёлся в истории",
        )
        .await;
    assert_eq!(
        refusal(r),
        InvestRefusal::OverWalletExcess {
            amount: dec!(300),
            room: dec!(3)
        }
    );
    assert_eq!(balance(&e).await, dec!(0), "отказ ничего не зачислил");
    e.engine
        .invest_resolve(
            id,
            &staff,
            &InvestVerdict::Happened {
                amount: Some(dec!(3)),
            },
            "чек нашёлся в истории",
        )
        .await
        .unwrap();
    assert_eq!(balance(&e).await, dec!(3));
    assert_eq!(get_move(&e, id).await.status, MoveStatus::Done);
    e.assert_books_match().await;
}

/// Шаг 5.1 (М-8, решение владельца 17.11): доля — от прибыли, а не от выручки. Потеря больше
/// комиссий — ничего не начислено, убыток переносится; следующие комиссии сначала покрывают его.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn profit_is_income_minus_losses_and_a_loss_carries_forward(pool: PgPool) {
    let e = env(pool).await;
    grant(&e, 0, dec!(0)).await;
    deposit(&e, CB, dec!(1000)).await;
    e.engine.invest_accrue().await.unwrap();

    // Комиссия заявки и потеря 30 в одном окне: прибыли нет.
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    accepted(e.submit(&url).await);
    e.drain().await;
    let fee = -e.ledger("revenue:fee:USDT").await;
    assert!(fee > Decimal::ZERO && fee < dec!(10), "{fee}");
    let mut conn = e.pool.acquire().await.unwrap();
    storage::ledger::post(
        &mut conn,
        &domain::ledger::loss(XR, domain::Asset::Usdt, dec!(10)).unwrap(),
        storage::ledger::PostingRefs {
            memo: Some("аккаунт ub-2 заблокирован, деньги признаны потерянными"),
            created_by: Some("staff:1"),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    drop(conn);
    // Потерю списали — в кошельке её действительно нет (иначе сверка нашла бы излишек).
    let have = e.world.balance(e.ub, XR, domain::Asset::Usdt);
    e.world
        .set_balance(e.ub, XR, domain::Asset::Usdt, have - dec!(10))
        .unwrap();
    let acc = e.engine.invest_accrue().await.unwrap();
    assert_eq!(acc.revenue, fee);
    assert_eq!(acc.losses, dec!(10));
    assert_eq!(acc.profit, Decimal::ZERO);
    assert!(
        acc.credited.is_empty(),
        "убыточное окно ничего не начисляет"
    );
    assert_eq!(balance(&e).await, dec!(1000));

    // Следующие комиссии сначала покрывают перенесённый убыток.
    let mut earned = Decimal::ZERO;
    let mut credited = Decimal::ZERO;
    for _ in 0..8 {
        let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
        accepted(e.submit(&url).await);
        e.drain().await;
        let before = -e.ledger("revenue:fee:USDT").await;
        let acc = e.engine.invest_accrue().await.unwrap();
        earned = before;
        credited += acc.credited.iter().map(|(_, a)| *a).sum::<Decimal>();
        assert!(acc.carried <= Decimal::ZERO);
        // Начисление никогда не больше прибыли окна × доля.
        assert!(
            acc.credited.iter().map(|(_, a)| *a).sum::<Decimal>()
                <= acc.profit * dec!(50) / dec!(100)
        );
    }
    // Всего начислено не больше (все комиссии − потеря) × доля капитала × 50 %.
    let ceiling = (earned - dec!(10)).max(Decimal::ZERO) * dec!(50) / dec!(100);
    assert!(credited <= ceiling, "{credited} > {ceiling}");
    assert!(credited > Decimal::ZERO, "убыток покрыт — начисления пошли");
    e.assert_books_match().await;
}

/// Д-19.6 (проверка 19.11; картинка last-fixes.png принята владельцем 20.11, вариант Б): чек
/// пополнения меньше минимума инвестора в инвестиции не идёт — его сумма возвращается чеком того
/// же кошелька. Раньше такой чек молча зачислялся, хотя экран обещал «от 10».
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_deposit_below_the_minimum_is_returned_by_check(pool: PgPool) {
    let e = env(pool).await;
    grant(&e, 0, dec!(100)).await;
    let dep = deposit(&e, CB, dec!(5)).await;
    let m = get_move(&e, dep).await;
    assert_eq!(m.status, MoveStatus::Refunded, "{m:?}");
    assert_eq!(m.amount, Some(dec!(5)));
    assert_eq!(balance(&e).await, dec!(0), "в капитал не попало");
    let checks = e.world.checks_created_by(e.ub);
    assert_eq!(checks.len(), 1, "один чек возврата");
    assert_eq!(checks[0].amount, usdt(dec!(5)));
    assert_eq!(checks[0].platform, CB, "тем же кошельком");
    assert_eq!(m.check_url.as_deref(), Some(checks[0].url.as_str()));
    assert_eq!(active_holds(&e).await, 0);
    e.world.claim_check(e.client, &checks[0].url).unwrap();
    e.assert_books_match().await;
    // Ровно минимум — обычное пополнение.
    let dep = deposit(&e, CB, dec!(10)).await;
    assert_eq!(get_move(&e, dep).await.status, MoveStatus::Done);
    assert_eq!(balance(&e).await, dec!(10));
}

/// Чек возврата создан, а ответ потерян («выполнил и вернул таймаут»): сверка находит его — один
/// чек, второго нет.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_lost_reply_on_the_return_check_ends_with_one_check(pool: PgPool) {
    let e = env(pool).await;
    grant(&e, 0, dec!(100)).await;
    e.world
        .inject(e.ub, Call::CheckPress, Fault::TimeoutAfterEffect, 1);
    let dep = deposit(&e, XR, dec!(4)).await;
    e.engine.resolve_unknown().await.unwrap();
    e.drain().await;
    let m = get_move(&e, dep).await;
    assert_eq!(m.status, MoveStatus::Refunded, "{m:?}");
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1, "второго чека нет");
    assert_eq!(balance(&e).await, dec!(0));
    assert_eq!(active_holds(&e).await, 0);
    e.assert_books_match().await;
}

/// Нажатие, которое создаёт чек возврата, не дошло (таймаут до действия): исход неясен, сверка
/// чека не находит — к владельцу, повтора нет. Владелец проверил — чека нет: возврат создаётся
/// заново, один чек.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_return_check_that_was_never_made_is_made_after_the_owner_checks(pool: PgPool) {
    let e = env(pool).await;
    let staff = grant(&e, 0, dec!(100)).await;
    e.world
        .inject(e.ub, Call::CheckPress, Fault::TimeoutBeforeEffect, 1);
    let dep = deposit(&e, CB, dec!(3)).await;
    let ops = {
        let mut conn = e.pool.acquire().await.unwrap();
        storage::operations::for_invest_move(&mut conn, dep)
            .await
            .unwrap()
    };
    assert!(
        ops.iter()
            .any(|o| o.kind == OpKind::CreateRefundCheck && o.invest_move_id == Some(dep)),
        "{ops:?}"
    );
    e.engine.resolve_unknown().await.unwrap();
    e.drain().await;
    let m = get_move(&e, dep).await;
    assert_eq!(m.status, MoveStatus::Manual, "{m:?}");
    assert!(
        e.world.checks_created_by(e.ub).is_empty(),
        "без человека не повторяем"
    );
    assert_eq!(active_holds(&e).await, 1, "деньги удержаны до решения");

    e.engine
        .invest_resolve(
            dep,
            &staff,
            &InvestVerdict::DidNotHappen,
            "проверил кошелёк, чека нет",
        )
        .await
        .unwrap();
    e.drain().await;
    let m = get_move(&e, dep).await;
    assert_eq!(m.status, MoveStatus::Refunded, "{m:?}");
    let checks = e.world.checks_created_by(e.ub);
    assert_eq!(checks.len(), 1, "один чек возврата");
    assert_eq!(checks[0].amount, usdt(dec!(3)));
    assert_eq!(balance(&e).await, dec!(0));
    assert_eq!(active_holds(&e).await, 0);
    e.assert_books_match().await;
}
