//! Веха 4.2: кошелёк уходит с заблокированного аккаунта на другой через вход с переносом. Учёт
//! кошелька тот же (остаток на старом аккаунте покажет сверка), перенос виден в журнале, а пока
//! деньги кошелька в полёте — отказ без изменений.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use app::login::{LoginCmdError, LoginTarget, register};
use domain::Platform;
use mtproto::{LoginOutcome, SealedSession};
use sqlx::PgPool;

const OWNER: i64 = 501;

fn outcome(user_id: i64) -> LoginOutcome {
    LoginOutcome {
        user_id,
        username: None,
        phone_masked: format!("+31*****{}", user_id % 100),
        sealed: SealedSession {
            key_version: 1,
            nonce: [7; 24],
            ciphertext: vec![9; 48],
        },
        already_authorized: false,
    }
}

async fn bound(pool: &PgPool, platform: Platform) -> String {
    let mut conn = pool.acquire().await.unwrap();
    storage::wallets::wallet_binding(&mut conn, platform)
        .await
        .unwrap()
        .unwrap()
        .userbot_label
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_wallet_moves_off_a_blocked_account(pool: PgPool) {
    {
        let mut conn = pool.acquire().await.unwrap();
        storage::staff::ensure_owner(&mut conn, OWNER, "Игнат")
            .await
            .unwrap();
    }
    let cb = Platform::CryptoBot;
    register(&pool, &LoginTarget::new("ub-1".into(), cb), &outcome(7001))
        .await
        .unwrap();
    let wallet_before = {
        let mut conn = pool.acquire().await.unwrap();
        storage::wallets::active(&mut conn, cb)
            .await
            .unwrap()
            .unwrap()
            .0
    };
    // Без переноса — прежний отказ: кошелёк с деньгами просто так не переезжает.
    let refused = register(&pool, &LoginTarget::new("ub-2".into(), cb), &outcome(7002)).await;
    assert!(
        matches!(refused, Err(LoginCmdError::WalletBoundElsewhere { .. })),
        "{refused:?}"
    );
    // Деньги в полёте (удержание живой заявки на этом кошельке) — отказ и с переносом.
    let mut target = LoginTarget::new("ub-2".into(), cb);
    target.move_wallet = true;
    let mut conn = pool.acquire().await.unwrap();
    storage::users::upsert(&mut conn, 111, &storage::users::Profile::default())
        .await
        .unwrap();
    let id = storage::orders::insert(
        &mut conn,
        &storage::orders::NewOrder {
            user_id: 111,
            direction: domain::Direction::CbToXrCheck,
            asset: domain::Asset::Usdt,
            intake_start_param: Some("CQmove2"),
            invoice_start_param: None,
        },
    )
    .await
    .unwrap()
    .id;
    storage::holds::place_unchecked(&mut conn, id, wallet_before, domain::Asset::Usdt, 5.into())
        .await
        .unwrap();
    drop(conn);
    let busy = register(&pool, &target, &outcome(7002)).await;
    assert!(
        matches!(&busy, Err(LoginCmdError::WalletBusy { blockers, .. }) if blockers.len() == 1),
        "{busy:?}"
    );
    assert_eq!(bound(&pool, cb).await, "ub-1");
    // Заявку разобрали — перенос проходит: тот же кошелёк (тот же учёт), новый аккаунт.
    sqlx::query("UPDATE holds SET state = 'released', closed_at = now()")
        .execute(&pool)
        .await
        .unwrap();
    let r = register(&pool, &target, &outcome(7002)).await.unwrap();
    assert_eq!(r.moved_from.as_deref(), Some("ub-1"));
    assert_eq!(r.wallet_id, wallet_before);
    assert_eq!(bound(&pool, cb).await, "ub-2");
    let mut conn = pool.acquire().await.unwrap();
    let audit = storage::audit::recent(&mut conn, 10, None, Some(("wallet", "cb")))
        .await
        .unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.action == "wallet.move" && a.staff_id == OWNER),
        "{audit:?}"
    );
}
