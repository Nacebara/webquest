//! Пульт и движок: личные условия клиента, ступени, лимит нового клиента, встречный поток
//! и решения персонала по заявке на ручном разборе. Симулятор кошельков + живой PostgreSQL;
//! после денежных сценариев балансы симулятора равны леджеру.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{CB, CLIENT, XR, accepted, env, usdt};
use domain::fees::{FeeMode, FeeSchedule, FeeTier};
use domain::fsm::OrderState;
use domain::{Asset, Decimal, Direction};
use engine::fees::{PromoEvent, PromoMode, PromoSettings};
use engine::{OperatorDecision, OperatorError, OperatorRequest, Refusal};
use rust_decimal_macros::dec;
use sqlx::PgPool;
use storage::admin::Thresholds;
use storage::operations::OpStatus;
use storage::staff::{Role, StaffMember};
use storage::user_terms::UserTerms;
use userbot::sim::{Call, CheckSpec, Fault};

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

async fn personal(pool: &PgPool, direction: Direction, edit: impl FnOnce(&mut UserTerms)) {
    let mut conn = pool.acquire().await.unwrap();
    let owner = storage::staff::ensure_owner(&mut conn, 1, "Владелец")
        .await
        .unwrap();
    let mut terms = storage::user_terms::get(&mut conn, CLIENT, direction)
        .await
        .unwrap();
    edit(&mut terms);
    storage::user_terms::save(&mut conn, CLIENT, &terms, owner.tg_user_id)
        .await
        .unwrap();
}

async fn set_target(pool: &PgPool, platform: domain::Platform, target: Decimal) {
    let mut conn = pool.acquire().await.unwrap();
    let (wallet, _) = storage::wallets::active(&mut conn, platform)
        .await
        .unwrap()
        .unwrap();
    storage::admin::set_thresholds(
        &mut conn,
        wallet,
        Asset::Usdt,
        Thresholds::from_target(target),
    )
    .await
    .unwrap();
}

async fn set_promo(pool: &PgPool, mode: PromoMode) {
    let mut conn = pool.acquire().await.unwrap();
    let owner = storage::staff::ensure_owner(&mut conn, 1, "Владелец")
        .await
        .unwrap();
    let s = PromoSettings {
        mode,
        ..PromoSettings::default()
    };
    engine::fees::save_promo_settings(&mut conn, &s, owner.tg_user_id)
        .await
        .unwrap();
}

async fn switch(pool: &PgPool, direction: Direction, on: bool) {
    let mut conn = pool.acquire().await.unwrap();
    let owner = storage::staff::ensure_owner(&mut conn, 1, "Владелец")
        .await
        .unwrap();
    engine::fees::switch_promo(&mut conn, direction, on, owner.tg_user_id)
        .await
        .unwrap();
}

async fn newcomer_limit(pool: &PgPool, on: bool) {
    let mut conn = pool.acquire().await.unwrap();
    storage::settings::set(
        &mut conn,
        storage::user_terms::NEWCOMER_LIMIT,
        &serde_json::json!({ "on": on }),
        None,
    )
    .await
    .unwrap();
}

// ---------------------------------------------------------------------------------------------
// Цены

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn personal_fee_replaces_the_base_rate(pool: PgPool) {
    let e = env(pool).await;
    personal(&e.pool, Direction::CbToXrCheck, |t| {
        t.fee_pct = Some(dec!(1))
    })
    .await;
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    let order = accepted(e.submit(&url).await);
    e.drain().await;
    let (order, d) = e.order(order.id).await;
    assert_eq!(order.state, OrderState::Completed);
    assert_eq!(d.fee_amount, Some(dec!(1)));
    assert_eq!(d.payout_amount, Some(dec!(99)));
    assert_eq!(d.fee_pct_applied, Some(dec!(1)));
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn tiers_pick_the_rate_by_amount_and_flat_mode_ignores_them(pool: PgPool) {
    let e = env(pool).await;
    {
        let mut conn = e.pool.acquire().await.unwrap();
        let mut dir = storage::directions::get(&mut conn, Direction::CbToXrCheck)
            .await
            .unwrap();
        dir.fee_mode = FeeMode::Tiers;
        dir.fee_tiers = Some(
            FeeSchedule::new(vec![
                FeeTier {
                    from: dec!(0),
                    fee_pct: dec!(2.5),
                },
                FeeTier {
                    from: dec!(50),
                    fee_pct: dec!(1.5),
                },
                FeeTier {
                    from: dec!(90),
                    fee_pct: dec!(1),
                },
            ])
            .unwrap(),
        );
        let owner = storage::staff::ensure_owner(&mut conn, 1, "Владелец")
            .await
            .unwrap();
        storage::directions::save_terms(&mut conn, &dir, owner.tg_user_id)
            .await
            .unwrap();
    }
    for (amount, fee) in [
        (dec!(20), dec!(0.5)),
        (dec!(60), dec!(0.9)),
        (dec!(100), dec!(1)),
    ] {
        let url = e.world.client_check(CB, usdt(amount)).unwrap();
        let order = accepted(e.submit(&url).await);
        e.drain().await;
        let (_, d) = e.order(order.id).await;
        assert_eq!(d.fee_amount, Some(fee), "{amount}");
    }
    // Назад на фикс: ступени хранятся, но не действуют.
    let mut conn = e.pool.acquire().await.unwrap();
    let mut dir = storage::directions::get(&mut conn, Direction::CbToXrCheck)
        .await
        .unwrap();
    dir.fee_mode = FeeMode::Flat;
    storage::directions::save_terms(&mut conn, &dir, 1)
        .await
        .unwrap();
    let dir = storage::directions::get(&mut conn, Direction::CbToXrCheck)
        .await
        .unwrap();
    assert_eq!(dir.fee_tiers.unwrap().tiers().len(), 3);
    drop(conn);
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    let order = accepted(e.submit(&url).await);
    e.drain().await;
    assert_eq!(e.order(order.id).await.1.fee_amount, Some(dec!(2.5)));
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn newcomer_limit_can_be_lifted_for_one_client(pool: PgPool) {
    let e = env(pool).await;
    // Включён (по умолчанию): 150 у нового клиента — возврат (как в engine.rs).
    let url = e.world.client_check(CB, usdt(dec!(150))).unwrap();
    let order = accepted(e.submit(&url).await);
    e.drain().await;
    assert_eq!(e.order(order.id).await.0.state, OrderState::Refunded);
    // Снят лично на другом направлении: там — выплата, клиент всё ещё новый.
    personal(&e.pool, Direction::XrToCbCheck, |t| {
        t.newcomer_exempt = true
    })
    .await;
    let url = e.world.client_check(XR, usdt(dec!(150))).unwrap();
    let order = accepted(e.submit(&url).await);
    e.drain().await;
    assert_eq!(e.order(order.id).await.0.state, OrderState::Completed);
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn newcomer_limit_switches_off_for_everyone(pool: PgPool) {
    let e = env(pool).await;
    newcomer_limit(&e.pool, false).await;
    let url = e.world.client_check(CB, usdt(dec!(150))).unwrap();
    let order = accepted(e.submit(&url).await);
    e.drain().await;
    assert_eq!(e.order(order.id).await.0.state, OrderState::Completed);
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn personal_maximum_refunds_bigger_checks(pool: PgPool) {
    let e = env(pool).await;
    personal(&e.pool, Direction::CbToXrCheck, |t| {
        t.max_amount = Some(dec!(40))
    })
    .await;
    let url = e.world.client_check(CB, usdt(dec!(50))).unwrap();
    let order = accepted(e.submit(&url).await);
    e.drain().await;
    let (order, d) = e.order(order.id).await;
    assert_eq!(order.state, OrderState::Refunded);
    assert_eq!(d.failure_code.as_deref(), Some("above_maximum"));
    e.assert_books_match().await;
}

// ---------------------------------------------------------------------------------------------
// Встречный поток

/// Перекос: на xRocket нехватка, на CryptoBot излишек. Встречный поток — xRocket → CryptoBot
/// (и оплата счетов CryptoBot); основному направлению выгодного курса не бывает.
async fn skew(pool: &PgPool, xr_target: Decimal) {
    set_target(pool, CB, dec!(400)).await;
    set_target(pool, XR, xr_target).await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn hint_mode_suggests_and_manual_switch_applies_to_the_rebalancing_volume_only(pool: PgPool) {
    let e = env(pool).await;
    newcomer_limit(&e.pool, false).await;
    // xRocket 1000 при цели 1150 (нехватка 13 %), CryptoBot 1000 при цели 400 (излишек 600).
    skew(&e.pool, dec!(1150)).await;
    // Нехватка меньше порога включения (30 %): подсказки нет.
    assert!(e.engine.promo_tick().await.unwrap().is_empty());
    switch(&e.pool, Direction::XrToCbCheck, true).await;
    // 200 USDT: 150 по выгодному курсу 0.3 %, 50 — по обычной ставке после рычага
    // (CryptoBot в излишке: 1.5 − 0.75 = 0.75 %). 0.45 + 0.375 = 0.825.
    let url = e.world.client_check(XR, usdt(dec!(200))).unwrap();
    let order = accepted(e.submit(&url).await);
    e.drain().await;
    let (order, d) = e.order(order.id).await;
    assert_eq!(order.state, OrderState::Completed);
    let fee = d.fee_amount.unwrap();
    assert!(fee >= dec!(0.825) && fee < dec!(0.835), "{fee}");
    assert_eq!(d.fee_pct_applied, Some(dec!(0.42)));
    // Обратное направление включить можно, но объёма у него нет — курс обычный.
    switch(&e.pool, Direction::CbToXrCheck, true).await;
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    let order = accepted(e.submit(&url).await);
    e.drain().await;
    // Только рычаг (xRocket теперь чуть выше цели: 2.5 − 0.03), выгодного курса 0.3 % нет.
    let fee = e.order(order.id).await.1.fee_amount.unwrap();
    assert_eq!(fee, dec!(2.47));
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn hint_mode_raises_one_hint_until_the_skew_is_gone(pool: PgPool) {
    let e = env(pool).await;
    // Нехватка xRocket 50 %.
    skew(&e.pool, dec!(2000)).await;
    let events = e.engine.promo_tick().await.unwrap();
    assert!(events.contains(&PromoEvent::Hint {
        direction: Direction::XrToCbCheck,
        room: dec!(600),
    }));
    // Повтор — то же событие, счётчик растёт, второго не появляется.
    e.engine.promo_tick().await.unwrap();
    let mut conn = e.pool.acquire().await.unwrap();
    let hints: Vec<_> = storage::alerts::open(&mut conn, 50)
        .await
        .unwrap()
        .into_iter()
        .filter(|a| a.dedup_key == "promo_hint:xr_to_cb_check")
        .collect();
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].count, 2);
    assert_eq!(hints[0].title, "promo_hint");
    // Перекос ушёл — подсказка закрывается сама.
    drop(conn);
    skew(&e.pool, dec!(1000)).await;
    e.engine.promo_tick().await.unwrap();
    let mut conn = e.pool.acquire().await.unwrap();
    assert!(
        storage::alerts::open(&mut conn, 50)
            .await
            .unwrap()
            .iter()
            .all(|a| a.dedup_key != "promo_hint:xr_to_cb_check")
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn auto_mode_starts_and_stops_with_hysteresis_and_respects_a_manual_off(pool: PgPool) {
    let e = env(pool).await;
    set_promo(&e.pool, PromoMode::Auto).await;
    skew(&e.pool, dec!(2000)).await;
    let events = e.engine.promo_tick().await.unwrap();
    assert!(events.contains(&PromoEvent::Started {
        direction: Direction::XrToCbCheck,
        room: dec!(600)
    }));
    assert!(events.contains(&PromoEvent::Started {
        direction: Direction::PayCbInvoice,
        room: dec!(600)
    }));
    assert!(
        !events
            .iter()
            .any(|ev| matches!(ev, PromoEvent::Started { direction, .. } if *direction == Direction::CbToXrCheck))
    );
    // Нехватка 13 %: выше порога выключения (10 %) — держим.
    skew(&e.pool, dec!(1150)).await;
    assert!(e.engine.promo_tick().await.unwrap().is_empty());
    // Нехватка 5 %: выключаем.
    skew(&e.pool, dec!(1050)).await;
    let events = e.engine.promo_tick().await.unwrap();
    assert!(events.contains(&PromoEvent::Stopped {
        direction: Direction::XrToCbCheck
    }));
    // Снова перекос, но владелец выключил руками: автоматика ждёт, пока перекос пройдёт.
    skew(&e.pool, dec!(2000)).await;
    switch(&e.pool, Direction::XrToCbCheck, false).await;
    let events = e.engine.promo_tick().await.unwrap();
    assert!(!events.contains(&PromoEvent::Started {
        direction: Direction::XrToCbCheck,
        room: dec!(600)
    }));
    skew(&e.pool, dec!(1000)).await;
    e.engine.promo_tick().await.unwrap();
    skew(&e.pool, dec!(2000)).await;
    let events = e.engine.promo_tick().await.unwrap();
    assert!(events.contains(&PromoEvent::Started {
        direction: Direction::XrToCbCheck,
        room: dec!(600)
    }));
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn off_mode_ignores_switched_on_directions(pool: PgPool) {
    let e = env(pool).await;
    skew(&e.pool, dec!(2000)).await;
    switch(&e.pool, Direction::XrToCbCheck, true).await;
    set_promo(&e.pool, PromoMode::Off).await;
    let url = e.world.client_check(XR, usdt(dec!(100))).unwrap();
    let order = accepted(e.submit(&url).await);
    e.drain().await;
    // Без выгодного курса: 1.5 % − 0.75 рычага = 0.75 %.
    assert_eq!(e.order(order.id).await.1.fee_amount, Some(dec!(0.75)));
}

// ---------------------------------------------------------------------------------------------
// Решения по заявке на разборе

/// Выплата не удалась (в кошельке xRocket 5 USDT вместо 1000): заявка на разборе, деньги
/// клиента проведены, прежняя попытка — `failed`.
async fn stuck_payout(e: &common::Env) -> i64 {
    e.world.set_balance(e.ub, XR, Asset::Usdt, dec!(5)).unwrap();
    let url = e.world.client_check(CB, usdt(dec!(100))).unwrap();
    let order = accepted(e.submit(&url).await);
    e.drain().await;
    assert_eq!(e.order(order.id).await.0.state, OrderState::ManualReview);
    e.world
        .set_balance(e.ub, XR, Asset::Usdt, dec!(1000))
        .unwrap();
    order.id
}

fn request<'a>(
    order_id: i64,
    decision: OperatorDecision,
    staff: &'a StaffMember,
) -> OperatorRequest<'a> {
    OperatorRequest {
        order_id,
        decision,
        staff,
        reason: "проверил кошелёк",
        double_confirmed: true,
    }
}

fn refused(r: Result<storage::orders::OrderRow, OperatorError>) -> Refusal {
    match r {
        Err(OperatorError::Refused(refusal)) => refusal,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn retry_payout_respects_the_manual_limit_and_pays_exactly_once(pool: PgPool) {
    let e = env(pool).await;
    let id = stuck_payout(&e).await;
    let operator = staff(&e.pool, 10, Role::Operator).await;
    let admin = staff(&e.pool, 20, Role::Admin).await;
    let retry = OperatorDecision::RetryPayout {
        previous_failed: false,
    };
    // Оператор — до 50 USDT, выплата 97.5.
    assert!(matches!(
        refused(
            e.engine
                .operator_decide(request(id, retry.clone(), &operator))
                .await
        ),
        Refusal::OverManualLimit { .. }
    ));
    // Причина обязательна.
    let mut short = request(id, retry.clone(), &admin);
    short.reason = "ок";
    assert_eq!(
        refused(e.engine.operator_decide(short).await),
        Refusal::ReasonTooShort
    );
    let order = e
        .engine
        .operator_decide(request(id, retry.clone(), &admin))
        .await
        .unwrap();
    assert_eq!(order.state, OrderState::PayoutPending);
    // Пока операция в работе — второе решение не принимается.
    assert!(matches!(
        refused(
            e.engine
                .operator_decide(request(id, retry.clone(), &admin))
                .await
        ),
        Refusal::NotInReview(OrderState::PayoutPending)
    ));
    e.drain().await;
    let (order, d) = e.order(id).await;
    assert_eq!(order.state, OrderState::Completed);
    assert_eq!(d.payout_amount, Some(dec!(97.5)));
    let ops = e.ops(id).await;
    let payouts: Vec<_> = ops
        .iter()
        .filter(|o| o.idempotency_key.contains("payout"))
        .collect();
    assert_eq!(payouts.len(), 2);
    assert_eq!(payouts[0].status, OpStatus::Failed);
    assert_eq!(payouts[1].idempotency_key, format!("ord-{id}-payout-2"));
    assert_eq!(payouts[1].status, OpStatus::Succeeded);
    assert_eq!(e.world.checks_created_by(e.ub).len(), 1);
    // Решение — в журнале, переход — от имени сотрудника.
    let mut conn = e.pool.acquire().await.unwrap();
    let audit = storage::audit::recent(&mut conn, 10, None, Some(("order", &id.to_string())))
        .await
        .unwrap();
    assert_eq!(audit[0].action, "order.retry_payout");
    assert_eq!(audit[0].staff_id, 20);
    assert!(audit[0].double_confirmed);
    let card = storage::admin::order_card(&mut conn, id).await.unwrap();
    assert!(card.transitions.iter().any(|t| t.actor == "staff:20"));
    drop(conn);
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn unknown_previous_payout_needs_an_explicit_confirmation(pool: PgPool) {
    let e = env(pool).await;
    let id = stuck_payout(&e).await;
    // Прежняя попытка с неясным исходом (как после таймаута без найденного чека).
    sqlx::query("UPDATE operations SET status = 'unknown' WHERE order_id = $1 AND kind = 'create_payout_check'")
        .bind(id)
        .execute(&e.pool)
        .await
        .unwrap();
    let owner = staff(&e.pool, 1, Role::Owner).await;
    let r = e
        .engine
        .operator_decide(request(
            id,
            OperatorDecision::RetryPayout {
                previous_failed: false,
            },
            &owner,
        ))
        .await;
    assert!(matches!(refused(r), Refusal::PreviousUnknown(_)));
    // Возврат при неясной выплате — тоже только с подтверждением.
    let r = e
        .engine
        .operator_decide(request(
            id,
            OperatorDecision::Refund {
                previous_failed: false,
            },
            &owner,
        ))
        .await;
    assert!(matches!(refused(r), Refusal::PreviousUnknown(_)));
    e.engine
        .operator_decide(request(
            id,
            OperatorDecision::RetryPayout {
                previous_failed: true,
            },
            &owner,
        ))
        .await
        .unwrap();
    e.drain().await;
    assert_eq!(e.order(id).await.0.state, OrderState::Completed);
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn refund_from_review_returns_the_intake_on_the_source_platform(pool: PgPool) {
    let e = env(pool).await;
    let id = stuck_payout(&e).await;
    let operator = staff(&e.pool, 10, Role::Operator).await;
    // Возврат 100 — выше потолка оператора.
    assert!(matches!(
        refused(
            e.engine
                .operator_decide(request(
                    id,
                    OperatorDecision::Refund {
                        previous_failed: false
                    },
                    &operator
                ))
                .await
        ),
        Refusal::OverManualLimit { .. }
    ));
    let admin = staff(&e.pool, 20, Role::Admin).await;
    e.engine
        .operator_decide(request(
            id,
            OperatorDecision::Refund {
                previous_failed: false,
            },
            &admin,
        ))
        .await
        .unwrap();
    e.drain().await;
    let (order, d) = e.order(id).await;
    assert_eq!(order.state, OrderState::Refunded);
    assert_eq!(d.refund_amount, Some(dec!(100)));
    let url = d.payout_check_url.unwrap();
    assert!(url.starts_with("https://t.me/send?start=CQ"));
    e.world.claim_check(e.client, &url).unwrap();
    assert_eq!(e.world.balance(e.client, CB, Asset::Usdt), dec!(100));
    assert_eq!(e.ledger("liability:payable:USDT").await, dec!(0));
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn close_as_paid_takes_the_check_from_the_evidence(pool: PgPool) {
    let e = env(pool).await;
    let id = stuck_payout(&e).await;
    // Сотрудник сам выдал клиенту чек на 97.5 из служебного аккаунта.
    let check = e
        .world
        .create_check(CheckSpec::new(XR, usdt(dec!(97.5))).by(e.ub))
        .unwrap();
    let evidence = format!("https://t.me/xrocket?start={}", check.code);
    let operator = staff(&e.pool, 10, Role::Operator).await;
    assert_eq!(
        refused(
            e.engine
                .operator_decide(request(
                    id,
                    OperatorDecision::CloseAsPaid {
                        evidence: evidence.clone()
                    },
                    &operator
                ))
                .await
        ),
        Refusal::Forbidden(Role::Admin)
    );
    let admin = staff(&e.pool, 20, Role::Admin).await;
    assert_eq!(
        refused(
            e.engine
                .operator_decide(request(
                    id,
                    OperatorDecision::CloseAsPaid {
                        evidence: " ".into()
                    },
                    &admin
                ))
                .await
        ),
        Refusal::EvidenceRequired
    );
    let order = e
        .engine
        .operator_decide(request(
            id,
            OperatorDecision::CloseAsPaid {
                evidence: evidence.clone(),
            },
            &admin,
        ))
        .await
        .unwrap();
    assert_eq!(order.state, OrderState::Completed);
    let payout = e
        .ops(id)
        .await
        .into_iter()
        .find(|o| o.idempotency_key.contains("payout"))
        .unwrap();
    assert_eq!(payout.status, OpStatus::Succeeded);
    assert_eq!(payout.external_id.as_deref(), Some(check.code.as_str()));
    assert_eq!(e.ledger("liability:payable:USDT").await, dec!(0));
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn close_without_payout_is_for_the_owner_only_and_keeps_the_money_with_a_reason(
    pool: PgPool,
) {
    let e = env(pool).await;
    let id = stuck_payout(&e).await;
    let admin = staff(&e.pool, 20, Role::Admin).await;
    assert_eq!(
        refused(
            e.engine
                .operator_decide(request(id, OperatorDecision::CloseWithoutPayout, &admin))
                .await
        ),
        Refusal::Forbidden(Role::Owner)
    );
    let owner = staff(&e.pool, 1, Role::Owner).await;
    let order = e
        .engine
        .operator_decide(request(id, OperatorDecision::CloseWithoutPayout, &owner))
        .await
        .unwrap();
    assert_eq!(order.state, OrderState::ClosedManual);
    assert_eq!(e.ledger("liability:payable:USDT").await, dec!(0));
    assert_eq!(e.ledger("revenue:retained:USDT").await, dec!(-100));
    let mut conn = e.pool.acquire().await.unwrap();
    assert_eq!(storage::holds::active(&mut conn, id).await.unwrap(), None);
    let memo: Option<String> = sqlx::query_scalar(
        "SELECT memo FROM ledger_transactions WHERE order_id = $1 AND kind = 'manual_close'",
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(memo.as_deref(), Some("проверил кошелёк"));
    drop(conn);
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn confirm_intake_then_pay_out_when_the_wallet_stayed_silent(pool: PgPool) {
    let e = env(pool).await;
    let url = e.world.client_check(XR, usdt(dec!(30))).unwrap();
    let order = accepted(e.submit(&url).await);
    // Деньги пришли, ответа нет: приход не проведён, заявка у человека.
    e.world.inject(e.ub, Call::SendText, Fault::NoReply, 1);
    e.drain().await;
    let id = order.id;
    assert_eq!(e.order(id).await.0.state, OrderState::ManualReview);
    let admin = staff(&e.pool, 20, Role::Admin).await;
    // Пока приход не подтверждён — ни выплаты, ни возврата.
    assert_eq!(
        refused(
            e.engine
                .operator_decide(request(
                    id,
                    OperatorDecision::RetryPayout {
                        previous_failed: false
                    },
                    &admin
                ))
                .await
        ),
        Refusal::FundsNotReceived
    );
    let operator = staff(&e.pool, 10, Role::Operator).await;
    assert_eq!(
        refused(
            e.engine
                .operator_decide(request(
                    id,
                    OperatorDecision::ConfirmIntake { amount: dec!(30) },
                    &operator
                ))
                .await
        ),
        Refusal::Forbidden(Role::Admin)
    );
    e.engine
        .operator_decide(request(
            id,
            OperatorDecision::ConfirmIntake { amount: dec!(30) },
            &admin,
        ))
        .await
        .unwrap();
    // Второй раз провести тот же приход нельзя.
    assert_eq!(
        refused(
            e.engine
                .operator_decide(request(
                    id,
                    OperatorDecision::ConfirmIntake { amount: dec!(30) },
                    &admin
                ))
                .await
        ),
        Refusal::IntakeAlreadyPosted
    );
    e.engine
        .operator_decide(request(
            id,
            OperatorDecision::RetryPayout {
                previous_failed: false,
            },
            &admin,
        ))
        .await
        .unwrap();
    e.drain().await;
    let (order, d) = e.order(id).await;
    assert_eq!(order.state, OrderState::Completed);
    assert_eq!(d.intake_amount, Some(dec!(30)));
    assert_eq!(d.payout_amount, Some(dec!(29.55)));
    e.assert_books_match().await;
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn paused_userbot_account_postpones_operations_without_spending_attempts(pool: PgPool) {
    let e = env(pool).await;
    let ub_id: i16 = sqlx::query_scalar("SELECT id FROM userbot_accounts LIMIT 1")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    {
        let mut conn = e.pool.acquire().await.unwrap();
        storage::admin::set_userbot_paused(&mut conn, ub_id, true)
            .await
            .unwrap();
    }
    let url = e.world.client_check(CB, usdt(dec!(20))).unwrap();
    let order = accepted(e.submit(&url).await);
    e.engine.process_due().await.unwrap();
    let (now, _) = e.order(order.id).await;
    assert_eq!(now.state, OrderState::IntakePending);
    let op = &e.ops(order.id).await[0];
    assert_eq!(op.status, OpStatus::Pending);
    assert_eq!(op.attempts, 0, "попытка не потрачена");
    assert!(
        e.world
            .messages(e.ub, userbot::Chat::WalletBot(CB))
            .iter()
            .all(|m| !m.out),
        "в кошелёк ничего не отправлено"
    );
    {
        let mut conn = e.pool.acquire().await.unwrap();
        storage::admin::set_userbot_paused(&mut conn, ub_id, false)
            .await
            .unwrap();
    }
    e.drain().await;
    assert_eq!(e.order(order.id).await.0.state, OrderState::Completed);
    e.assert_books_match().await;
}
