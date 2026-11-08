//! Почему направление не принимает: одна правда для пульта и клиента (симулятор + PostgreSQL).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{CB, XR, env};
use domain::Direction;
use engine::availability::Blocker;
use rust_decimal_macros::dec;
use sqlx::PgPool;

async fn blockers(e: &common::Env, d: Direction) -> Vec<Blocker> {
    e.engine
        .availability()
        .await
        .unwrap()
        .into_iter()
        .find(|a| a.direction == d)
        .unwrap()
        .blockers
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn every_pause_has_its_reason(pool: PgPool) {
    let e = env(pool).await;
    let all = e.engine.availability().await.unwrap();
    let cb_xr = all
        .iter()
        .find(|a| a.direction == Direction::CbToXrCheck)
        .unwrap();
    assert!(cb_xr.accepting(), "{cb_xr:?}");
    assert!(cb_xr.max_now.is_some());
    // Счета в общем окружении закрыты владельцем.
    assert!(
        blockers(&e, Direction::PayCbInvoice)
            .await
            .contains(&Blocker::Closed)
    );

    // Резерва xRocket не хватает на минимальную заявку CryptoBot → xRocket.
    sqlx::query(
        "UPDATE directions SET min_amount = 2000, max_amount = 5000 WHERE code = 'cb_to_xr_check'",
    )
    .execute(&e.pool)
    .await
    .unwrap();
    assert_eq!(
        blockers(&e, Direction::CbToXrCheck).await,
        vec![Blocker::NoReserve {
            platform: XR,
            available: dec!(1000),
            needed: dec!(2000),
        }]
    );

    // Юзербот на паузе — обе стороны, где он работает.
    sqlx::query("UPDATE userbot_accounts SET status = 'disabled'")
        .execute(&e.pool)
        .await
        .unwrap();
    let b = blockers(&e, Direction::XrToCbCheck).await;
    assert!(
        b.contains(&Blocker::UserbotPaused { platform: XR }),
        "{b:?}"
    );
    assert!(
        b.contains(&Blocker::UserbotPaused { platform: CB }),
        "{b:?}"
    );
    sqlx::query("UPDATE userbot_accounts SET status = 'active'")
        .execute(&e.pool)
        .await
        .unwrap();

    // Обслуживание, стоп-кран, автопауза с причиной.
    let mut conn = e.pool.acquire().await.unwrap();
    for key in ["maintenance", "kill_switch_payouts"] {
        storage::settings::set(&mut conn, key, &serde_json::json!({ "on": true }), None)
            .await
            .unwrap();
    }
    storage::directions::auto_pause_platform(
        &mut conn,
        CB,
        storage::directions::Side::Target,
        "сверка: не хватает 5 USDT",
    )
    .await
    .unwrap();
    drop(conn);
    let b = blockers(&e, Direction::XrToCbCheck).await;
    assert!(b.contains(&Blocker::Maintenance), "{b:?}");
    assert!(b.contains(&Blocker::KillSwitch), "{b:?}");
    assert!(
        b.contains(&Blocker::AutoPaused {
            reason: Some("сверка: не хватает 5 USDT".into())
        }),
        "{b:?}"
    );
}
