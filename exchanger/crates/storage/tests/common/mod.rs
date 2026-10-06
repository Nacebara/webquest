//! Общие фикстуры интеграционных тестов: четыре кошелька, счета леджера, капитал, клиент.

#![allow(dead_code, clippy::unwrap_used)]

use domain::ledger::{self, AccountCode};
use domain::{Asset, Decimal, Platform, WalletKind, WalletRef};
use sqlx::PgPool;
use storage::ledger::PostingRefs;
use storage::users::Profile;

pub const CLIENT: i64 = 111;
pub const CB_APP: WalletRef = WalletRef::new(Platform::CryptoBot, WalletKind::App);
pub const CB_PERSONAL: WalletRef = WalletRef::new(Platform::CryptoBot, WalletKind::Personal);
pub const XR_APP: WalletRef = WalletRef::new(Platform::XRocket, WalletKind::App);
pub const XR_PERSONAL: WalletRef = WalletRef::new(Platform::XRocket, WalletKind::Personal);

/// Кошельки, счета леджера USDT, клиент и 1 000 USDT капитала на `cb:app`.
pub async fn setup(pool: &PgPool) {
    let mut tx = pool.begin().await.unwrap();
    let ub_xr = storage::wallets::ensure_userbot(&mut tx, "ub-xr-1", "+31******01")
        .await
        .unwrap();
    let ub_cb = storage::wallets::ensure_userbot(&mut tx, "ub-cb-1", "+31******02")
        .await
        .unwrap();
    for (wallet, ub) in [
        (CB_PERSONAL, Some(ub_cb)),
        (CB_APP, None),
        (XR_PERSONAL, Some(ub_xr)),
        (XR_APP, None),
    ] {
        let wallet_id = storage::wallets::ensure_wallet(&mut tx, wallet, ub)
            .await
            .unwrap();
        storage::ledger::ensure_account(
            &mut tx,
            &AccountCode::wallet(wallet, Asset::Usdt),
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
    let capital = ledger::capital_in(CB_APP, Asset::Usdt, Decimal::from(1000)).unwrap();
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
