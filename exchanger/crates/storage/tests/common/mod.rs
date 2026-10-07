//! Общие фикстуры интеграционных тестов: два аккаунта юзербота с кошельками `cb` и `xr`,
//! счета леджера, по 1 000 USDT капитала на каждом кошельке, клиент.

#![allow(dead_code, clippy::unwrap_used)]

use domain::ledger::{self, AccountCode};
use domain::{Asset, Decimal, Platform};
use sqlx::PgPool;
use storage::ledger::PostingRefs;
use storage::users::Profile;

pub const CLIENT: i64 = 111;
pub const CB: Platform = Platform::CryptoBot;
pub const XR: Platform = Platform::XRocket;

pub async fn setup(pool: &PgPool) {
    let mut tx = pool.begin().await.unwrap();
    for (platform, label, phone) in [
        (CB, "ub-cb-1", "+31******01"),
        (XR, "ub-xr-1", "+31******02"),
    ] {
        let ub = storage::wallets::ensure_userbot(&mut tx, label, phone)
            .await
            .unwrap();
        let wallet_id = storage::wallets::ensure_wallet(&mut tx, platform, ub)
            .await
            .unwrap();
        storage::ledger::ensure_account(
            &mut tx,
            &AccountCode::wallet(platform, Asset::Usdt),
            Asset::Usdt,
            Some(wallet_id),
        )
        .await
        .unwrap();
    }
    for code in [
        AccountCode::payable(Asset::Usdt),
        AccountCode::revenue(ledger::Revenue::Fee, Asset::Usdt),
        AccountCode::revenue(ledger::Revenue::Excess, Asset::Usdt),
        AccountCode::equity(ledger::Equity::Capital, Asset::Usdt),
    ] {
        storage::ledger::ensure_account(&mut tx, &code, Asset::Usdt, None)
            .await
            .unwrap();
    }
    for platform in [CB, XR] {
        let capital = ledger::capital_in(platform, Asset::Usdt, Decimal::from(1000)).unwrap();
        storage::ledger::post(
            &mut tx,
            &capital,
            PostingRefs {
                memo: Some("initial capital"),
                created_by: Some("staff:1"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    storage::users::upsert(
        &mut tx,
        CLIENT,
        &Profile {
            username: Some("client"),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

/// Текст ошибки БД (с именем ограничения), чтобы проверять, какое именно правило сработало.
pub fn db_error_text(err: &sqlx::Error) -> String {
    match err {
        sqlx::Error::Database(db) => {
            format!("{} [{}]", db.message(), db.constraint().unwrap_or("-"))
        }
        other => other.to_string(),
    }
}
