//! Crash-тест движка (HANDOVER §5, M3.4): 200 заявок в обе стороны со случайными сбоями
//! транспорта и «падениями» посреди операций, затем восстановление.
//!
//! Инварианты в конце:
//! - нет операций `pending` / `dispatched`, нет заявок в промежуточных состояниях — каждая
//!   закрыта или ждёт человека (`MANUAL_REVIEW`);
//! - на заявку не больше одного чека выплаты или возврата: чеки юзербота, не привязанные к
//!   успешной операции, есть только у операций с неизвестным исходом, и не больше одного;
//! - чек выплаты = сумма выплаты, чек возврата = сумма прихода;
//! - деньги сходятся до копейки: баланс кошелька в симуляторе − баланс по леджеру = деньги,
//!   которые ушли человеку на разбор (пришли без проводки или ушли в чек без проводки).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use common::{CB, XR, env_with, usdt};
use domain::fsm::{OpKind, OrderState};
use domain::{Asset, Decimal, Platform};
use sqlx::PgPool;
use storage::operations::OpStatus;
use userbot::WalletBot;
use userbot::flows::FlowConfig;
use userbot::sim::{Call, Fault};

const ORDERS: usize = 200;

/// Детерминированный ГПСЧ (splitmix64): прогон воспроизводим.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

fn random_fault(rng: &mut Rng) -> (Call, Fault) {
    let ms = Duration::from_millis;
    match rng.below(14) {
        0 => (Call::SendText, Fault::TimeoutAfterEffect),
        1 => (Call::SendText, Fault::TimeoutBeforeEffect),
        2 => (Call::SendText, Fault::DisconnectedAfterEffect),
        3 => (Call::SendText, Fault::NoReply),
        4 => (Call::SendText, Fault::DelayReply(ms(20))),
        5 => (Call::SendText, Fault::DelayReply(ms(90))),
        6 => (
            Call::SendText,
            Fault::ProcessingFirst {
                final_after: ms(20),
            },
        ),
        7 => (Call::SendText, Fault::FloodWait(Duration::ZERO)),
        8 => (Call::SendInline, Fault::TimeoutAfterEffect),
        9 => (Call::SendInline, Fault::DisconnectedAfterEffect),
        10 => (Call::SendInline, Fault::TimeoutBeforeEffect),
        11 => (
            Call::InlineQuery,
            Fault::Rpc {
                code: 400,
                name: "BOT_INLINE_DISABLED".to_owned(),
            },
        ),
        12 => (Call::History, Fault::Disconnected),
        _ => (Call::Press, Fault::TimeoutAfterEffect),
    }
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn two_hundred_orders_with_faults_and_crashes_keep_money_exact(pool: PgPool) {
    let e = env_with(
        pool,
        FlowConfig {
            reply_timeout: Duration::from_millis(50),
            min_interval: Duration::ZERO,
            bot_actions_per_minute: 1_000_000,
            late_reply_quarantine: Duration::from_millis(20),
            xrocket_not_found_retry: Some(Duration::from_millis(5)),
            ..FlowConfig::default()
        },
    )
    .await;
    e.world
        .set_xrocket_fresh_not_found(Some(Duration::from_millis(300)));
    let mut rng = Rng(0x00C0_FFEE);
    let mut crashes = 0;
    let mut acted_before_crash = 0;

    for _ in 0..ORDERS {
        let platform = if rng.chance(50) { CB } else { XR };
        let cents = i64::try_from(200 + rng.below(9_000)).unwrap();
        let url = e
            .world
            .client_check(platform, usdt(Decimal::new(cents, 2)))
            .unwrap();
        for _ in 0..rng.below(3) {
            let (call, fault) = random_fault(&mut rng);
            e.world.inject(e.ub, call, fault, 1);
        }
        let _ = e.submit(&url).await;
        // «Падение»: операцию взяли в работу, а процесс умер до действия или — хуже — после
        // действия юзербота, но до записи итога.
        if rng.chance(25) {
            crashes += 1;
            let mut conn = e.pool.acquire().await.unwrap();
            let claimed = storage::operations::claim_due(&mut conn, 1).await.unwrap();
            drop(conn);
            if let Some(op) = claimed.first().filter(|_| rng.chance(60)) {
                acted_before_crash += 1;
                act(&e, op).await;
            }
            e.engine.recover().await.unwrap();
        }
        drain(&e).await;
        if rng.chance(10) {
            crashes += 1;
            e.engine.recover().await.unwrap();
        }
        e.world.clear_faults(e.ub);
    }
    e.engine.recover().await.unwrap();
    e.engine.resolve_unknown().await.unwrap();
    drain(&e).await;
    assert!(crashes > 10, "сценарий должен включать падения");
    assert!(acted_before_crash > 5, "и падения после действия юзербота");

    let mut conn = e.pool.acquire().await.unwrap();

    // 1. Ничего не висит.
    let open_ops: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations WHERE status IN ('pending', 'dispatched')",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(open_ops, 0, "операции не должны зависать");
    let orders: Vec<(i64,)> = sqlx::query_as("SELECT id FROM orders ORDER BY id")
        .fetch_all(&mut *conn)
        .await
        .unwrap();
    let mut by_state: HashMap<OrderState, usize> = HashMap::new();
    for (id,) in &orders {
        let o = storage::orders::get(&mut conn, *id, false).await.unwrap();
        assert!(
            o.state.is_terminal() || o.state == OrderState::ManualReview,
            "order {id} stuck in {:?}",
            o.state
        );
        *by_state.entry(o.state).or_default() += 1;
    }
    println!("states: {by_state:?}, crashes: {crashes}, acted before crash: {acted_before_crash}");
    assert!(by_state.get(&OrderState::Completed).copied().unwrap_or(0) > ORDERS / 3);

    // 2. Чеки юзербота и операции.
    let created = e.world.checks_created_by(e.ub);
    let mut linked: HashSet<String> = HashSet::new();
    let mut unknown_settles = 0;
    // Сколько денег ушло мимо леджера и пришло мимо леджера, по платформам.
    let mut off_books: HashMap<Platform, Decimal> = HashMap::new();
    for (id,) in &orders {
        let o = storage::orders::get(&mut conn, *id, false).await.unwrap();
        let d = storage::orders::details(&mut conn, *id).await.unwrap();
        let ops = storage::operations::for_order(&mut conn, *id)
            .await
            .unwrap();
        let settles: Vec<_> = ops
            .iter()
            .filter(|op| {
                matches!(
                    op.kind,
                    OpKind::CreatePayoutCheck | OpKind::CreateRefundCheck
                )
            })
            .collect();
        assert!(
            settles.len() <= 1,
            "order {id}: {} settle operations",
            settles.len()
        );
        for op in &settles {
            match op.status {
                OpStatus::Succeeded => {
                    let code = op.external_id.clone().expect("check code");
                    assert!(linked.insert(code.clone()), "check {code} linked twice");
                    let check = created
                        .iter()
                        .find(|c| c.code == code)
                        .expect("linked check exists");
                    assert_eq!(Some(check.amount.amount()), op.amount);
                    let expected = match op.kind {
                        OpKind::CreatePayoutCheck => d.payout_amount,
                        _ => d.refund_amount,
                    };
                    assert_eq!(op.amount, expected, "order {id}: check amount");
                    if op.kind == OpKind::CreateRefundCheck {
                        assert_eq!(d.refund_amount, d.intake_amount, "refund = intake");
                    }
                }
                OpStatus::Unknown => {
                    unknown_settles += 1;
                    assert_eq!(o.state, OrderState::ManualReview);
                }
                OpStatus::Failed => assert_eq!(o.state, OrderState::ManualReview),
                other => panic!("order {id}: settle operation {other:?}"),
            }
        }
        // Чек клиента зачислен юзерботу, а проводки прихода нет — деньги у человека на разборе.
        let param = d.intake_start_param.clone().unwrap();
        let claimed_by_us = e
            .world
            .check(&param)
            .is_some_and(|c| c.claimed_by.contains(&e.ub));
        let intake_posted: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM ledger_transactions WHERE order_id = $1 AND kind = 'intake')",
        )
        .bind(id)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        if claimed_by_us && !intake_posted {
            assert_eq!(
                o.state,
                OrderState::ManualReview,
                "order {id}: money in, no posting"
            );
            let check = e.world.check(&param).unwrap();
            *off_books.entry(o.direction.source()).or_default() += check.amount.amount();
        }
        if intake_posted {
            assert!(claimed_by_us, "order {id}: posted intake without the money");
        }
    }
    let unlinked: Vec<_> = created
        .iter()
        .filter(|c| !linked.contains(&c.code))
        .collect();
    assert!(
        unlinked.len() <= unknown_settles,
        "{} checks without a recorded operation, {} unknown operations",
        unlinked.len(),
        unknown_settles
    );
    for c in &unlinked {
        *off_books.entry(c.platform).or_default() -= c.amount.amount();
    }

    // 3. Деньги до копейки.
    for (platform, code) in [(CB, "asset:cb:USDT"), (XR, "asset:xr:USDT")] {
        let books = e.ledger(code).await;
        let wallet = e.world.balance(e.ub, platform, Asset::Usdt);
        let expected = off_books.get(&platform).copied().unwrap_or_default();
        assert_eq!(
            wallet - books,
            expected,
            "{platform:?}: wallet {wallet}, books {books}"
        );
    }
    assert!(e.ledger("liability:payable:USDT").await <= Decimal::ZERO);
}

/// Выполнить действие операции так, как сделал бы воркер, но не записывать итог.
async fn act(e: &common::Env, op: &storage::operations::OperationRow) {
    let wallet = &e.wallets[&op.platform];
    let tag = userbot::OpTag {
        key: op.idempotency_key.clone(),
        random_id: op.random_id,
    };
    match op.kind {
        OpKind::ActivateCheck => {
            let _ = wallet
                .activate_check(op.start_param.as_deref().unwrap(), &tag)
                .await;
        }
        OpKind::CreatePayoutCheck | OpKind::CreateRefundCheck => {
            let _ = wallet.issue_check(&usdt(op.amount.unwrap()), &tag).await;
        }
        OpKind::PayInvoice => {}
    }
}

async fn drain(e: &common::Env) {
    for _ in 0..100 {
        if e.engine.process_due().await.unwrap() == 0 {
            return;
        }
    }
    panic!("operations keep coming");
}
