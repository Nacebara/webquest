//! «Спасти резерв» (решение владельца 17.11.2026, вариант Б): движок + юзербот на симуляторе +
//! живой PostgreSQL. После каждого сценария: ровно один чек на спасение, леджер сходится с
//! балансами симулятора.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::{CB, XR, accepted, env, usdt};
use domain::fsm::OrderState;
use engine::rescue::{RESCUE_PAUSE, RescueError, RescueRefusal};
use rust_decimal_macros::dec;
use sqlx::PgPool;
use storage::rescues::RescueStatus;
use storage::staff::{Role, StaffMember};
use userbot::WalletBot;
use userbot::sim::{Call, Fault};

async fn staff(pool: &PgPool, id: i64, role: Role) -> StaffMember {
    let mut conn = pool.acquire().await.unwrap();
    if role == Role::Owner {
        storage::staff::ensure_owner(&mut conn, id, "Владелец")
            .await
            .unwrap();
    } else {
        let owner = storage::staff::ensure_owner(&mut conn, 1, "Владелец")
            .await
            .unwrap();
        storage::staff::add(&mut conn, id, role, "Сотрудник", owner.tg_user_id)
            .await
            .unwrap();
    }
    storage::staff::get(&mut conn, id).await.unwrap().unwrap()
}

fn refused(r: Result<impl std::fmt::Debug, RescueError>) -> RescueRefusal {
    match r {
        Err(RescueError::Refused(x)) => x,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

async fn paused(e: &common::Env) -> Vec<(String, Option<String>)> {
    sqlx::query_as("SELECT code, pause_reason FROM directions WHERE auto_paused ORDER BY code")
        .fetch_all(&e.pool)
        .await
        .unwrap()
}

async fn open_alert(e: &common::Env, key: &str) -> Option<serde_json::Value> {
    sqlx::query_scalar(
        "SELECT payload FROM alerts WHERE dedup_key = $1 AND resolved_at IS NULL AND acked_at IS NULL",
    )
    .bind(key)
    .fetch_optional(&e.pool)
    .await
    .unwrap()
}

/// Владелец спасает резерв xRocket: чек на весь баланс, ссылка — в тревоге владельцу,
/// проводка `capital_out`, кошелёк на паузе, леджер = кошелёк (0 и 0).
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn owner_rescues_the_whole_wallet_into_one_check(pool: PgPool) {
    let e = env(pool).await;
    let owner = staff(&e.pool, 1, Role::Owner).await;
    let r = e
        .engine
        .rescue_request(XR, &owner, Some("чужой вход"))
        .await
        .unwrap();
    assert_eq!(r.rescue.amount, dec!(1000));
    assert!(!r.capped);
    assert_eq!(r.in_flight, 0);
    // Пауза — сразу, до выплаты чека: новые заявки через xRocket не начнутся.
    assert!(
        paused(&e)
            .await
            .iter()
            .all(|(_, why)| why.as_deref() == Some(RESCUE_PAUSE))
    );
    e.drain().await;

    let checks = e.world.checks_created_by(e.ub);
    assert_eq!(checks.len(), 1, "ровно один чек");
    assert_eq!(e.world.balance(e.ub, XR, domain::Asset::Usdt), dec!(0));
    let mut conn = e.pool.acquire().await.unwrap();
    let done = storage::rescues::get(&mut conn, r.rescue.id, false)
        .await
        .unwrap();
    drop(conn);
    assert_eq!(done.status, RescueStatus::Issued);
    assert_eq!(done.check_url.as_deref(), Some(checks[0].url.as_str()));
    let alert = open_alert(&e, &format!("rescue:{}", r.rescue.id))
        .await
        .expect("тревога владельцу со ссылкой");
    assert_eq!(alert["url"].as_str(), Some(checks[0].url.as_str()));
    e.assert_books_match().await;

    // Пока нет пополнения, повторить нечего — меньше минимального чека.
    assert!(matches!(
        refused(e.engine.rescue_request(XR, &owner, None).await),
        RescueRefusal::Nothing { .. }
    ));
}

/// Не владелец — отказ, ничего не создано и не поставлено на паузу.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn only_the_owner_can_rescue(pool: PgPool) {
    let e = env(pool).await;
    for role in [Role::Admin, Role::Operator] {
        let who = staff(&e.pool, 20 + role as i64, role).await;
        assert_eq!(
            refused(e.engine.rescue_request(XR, &who, None).await),
            RescueRefusal::Forbidden
        );
    }
    assert!(paused(&e).await.is_empty());
    assert!(e.world.checks_created_by(e.ub).is_empty());
}

/// Чек создан, а ответ кошелька потерян («выполнил и вернул таймаут»): сверка находит
/// созданный чек — второго нет, ссылка у владельца.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_lost_reply_is_reconciled_to_exactly_one_check(pool: PgPool) {
    let e = env(pool).await;
    let owner = staff(&e.pool, 1, Role::Owner).await;
    let r = e.engine.rescue_request(XR, &owner, None).await.unwrap();
    e.world.inject(
        e.ub,
        Call::CheckPress,
        Fault::DelayReply(Duration::from_millis(700)),
        1,
    );
    e.world
        .inject(e.ub, Call::History, Fault::DelayReply(Duration::ZERO), 1);
    e.world.inject(e.ub, Call::History, Fault::Disconnected, 3);
    e.drain().await;
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1);
    tokio::time::sleep(Duration::from_millis(800)).await;
    e.world.clear_faults(e.ub);
    e.engine.resolve_unknown().await.unwrap();
    e.drain().await;

    assert_eq!(e.world.checks_created_by(e.ub).len(), 1, "второго чека нет");
    let mut conn = e.pool.acquire().await.unwrap();
    let done = storage::rescues::get(&mut conn, r.rescue.id, false)
        .await
        .unwrap();
    drop(conn);
    assert_eq!(done.status, RescueStatus::Issued);
    e.assert_books_match().await;
}

/// Чек создан, а найти его нельзя: исход неясен — решает владелец. «Чек был» — вывод
/// проведён; повторное решение отклоняется; второго чека нет.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn an_unclear_rescue_waits_for_the_owner(pool: PgPool) {
    let e = env(pool).await;
    let owner = staff(&e.pool, 1, Role::Owner).await;
    let r = e.engine.rescue_request(XR, &owner, None).await.unwrap();
    // «Подтверждаю» создало чек, ответа нет вовсе: поиск ничего не находит.
    e.world.inject(e.ub, Call::CheckPress, Fault::NoReply, 1);
    e.drain().await;
    e.engine.resolve_unknown().await.unwrap();
    e.world.clear_faults(e.ub);
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1);
    let mut conn = e.pool.acquire().await.unwrap();
    let unclear = storage::rescues::get(&mut conn, r.rescue.id, false)
        .await
        .unwrap();
    drop(conn);
    assert_eq!(unclear.status, RescueStatus::Unknown);
    assert!(
        open_alert(&e, &format!("rescue_unknown:{}", r.rescue.id))
            .await
            .is_some()
    );
    // Пока ждём решения владельца, второе спасение того же кошелька не начинается.
    assert_eq!(
        refused(e.engine.rescue_request(XR, &owner, None).await),
        RescueRefusal::AlreadyOpen
    );
    let admin = staff(&e.pool, 20, Role::Admin).await;
    assert_eq!(
        refused(e.engine.rescue_resolve(r.rescue.id, &admin, true).await),
        RescueRefusal::Forbidden
    );
    let done = e
        .engine
        .rescue_resolve(r.rescue.id, &owner, true)
        .await
        .unwrap();
    assert_eq!(done.status, RescueStatus::Issued);
    assert_eq!(
        refused(e.engine.rescue_resolve(r.rescue.id, &owner, false).await),
        RescueRefusal::WrongState
    );
    assert!(
        open_alert(&e, &format!("rescue_unknown:{}", r.rescue.id))
            .await
            .is_none()
    );
    e.drain().await;
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1, "второго чека нет");
    e.assert_books_match().await;
}

/// CryptoBot: в чек — не больше запаса до «лимита без код-пароля», иначе бот спросит код.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn cryptobot_rescue_stays_under_the_passcode_free_limit(pool: PgPool) {
    let e = env(pool).await;
    {
        let mut conn = e.pool.acquire().await.unwrap();
        storage::settings::set(
            &mut conn,
            "cb_passcode_free_limit_usd",
            &serde_json::json!(500),
            None,
        )
        .await
        .unwrap();
    }
    e.world
        .set_passcode_free_limit(e.ub, Some(dec!(500)))
        .unwrap();
    let owner = staff(&e.pool, 1, Role::Owner).await;
    let r = e.engine.rescue_request(CB, &owner, None).await.unwrap();
    // 95 % от 500 — запас на траты руками и курс (DESIGN-v0.2 Р11).
    assert_eq!(r.rescue.amount, dec!(475));
    assert!(r.capped);
    assert_eq!(r.rescue.measured, dec!(1000));
    e.drain().await;
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1);
    assert_eq!(e.world.balance(e.ub, CB, domain::Asset::Usdt), dec!(525));
    e.assert_books_match().await;
}

/// Стоп-кран останавливает выплаты клиентам, но не спасение; выплата заявки, начатой до
/// спасения, когда денег уже нет, уходит на разбор, а не теряется.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn rescue_goes_through_the_kill_switch_and_a_late_payout_goes_to_review(pool: PgPool) {
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
    let set_kill = |on: bool| {
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
    // Заявка CryptoBot → xRocket: чек клиента принят, выплата xRocket в очереди.
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    let order = accepted(e.submit(&url).await);
    assert_eq!(one.process_due().await.unwrap(), 1, "только активация");
    assert_eq!(e.order(order.id).await.0.state, OrderState::PayoutPending);
    set_kill(true).await;

    let owner = staff(&e.pool, 1, Role::Owner).await;
    let r = one.rescue_request(XR, &owner, None).await.unwrap();
    assert_eq!(r.in_flight, 1, "выплата в очереди");
    // Спасение уходит несмотря на стоп-кран; выплата клиента стоит.
    for _ in 0..4 {
        one.process_due().await.unwrap();
    }
    let mut conn = e.pool.acquire().await.unwrap();
    assert_eq!(
        storage::rescues::get(&mut conn, r.rescue.id, false)
            .await
            .unwrap()
            .status,
        RescueStatus::Issued
    );
    drop(conn);
    assert_eq!(e.order(order.id).await.0.state, OrderState::PayoutPending);

    set_kill(false).await;
    sqlx::query("UPDATE operations SET next_attempt_at = now() WHERE status = 'pending'")
        .execute(&e.pool)
        .await
        .unwrap();
    e.drain().await;
    // Денег в xRocket нет: выплата не создала чек, заявка — у человека.
    assert_eq!(e.order(order.id).await.0.state, OrderState::ManualReview);
    assert_eq!(
        e.world.checks_created_by(e.ub).len(),
        1,
        "только чек спасения"
    );
    e.assert_books_match().await;
}
