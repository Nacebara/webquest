//! Общая обвязка сценариев движка: PostgreSQL с двумя кошельками по 1000 USDT капитала,
//! симулятор @send и @xrocket, движок с юзерботами на симуляторе.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use domain::ledger::{self, AccountCode};
use domain::{Asset, Decimal, Direction, Money, Platform};
use engine::{Engine, EngineConfig, Submitted};
use rust_decimal_macros::dec;
use sqlx::PgPool;
use storage::ledger::PostingRefs;
use userbot::flows::FlowConfig;
use userbot::sim::{AccountId, SimTransport, SimWorld};
use userbot::{AccountGate, UserbotWallet, WalletBot};

pub const CB: Platform = Platform::CryptoBot;
pub const XR: Platform = Platform::XRocket;
pub const CLIENT: i64 = 111;

pub fn usdt(x: Decimal) -> Money {
    Money::new(x, Asset::Usdt).unwrap()
}

pub struct Env {
    pub pool: PgPool,
    pub world: SimWorld,
    pub ub: AccountId,
    pub client: AccountId,
    pub engine: Engine,
    pub wallets: HashMap<Platform, Arc<UserbotWallet<SimTransport>>>,
}

pub fn fast() -> FlowConfig {
    FlowConfig {
        reply_timeout: Duration::from_millis(300),
        min_interval: Duration::ZERO,
        bot_actions_per_minute: 10_000,
        late_reply_quarantine: Duration::from_millis(100),
        xrocket_not_found_retry: Some(Duration::from_millis(10)),
        ..FlowConfig::default()
    }
}

/// Два кошелька по 1000 USDT капитала (в леджере и в симуляторе), направления открыты.
pub async fn env(pool: PgPool) -> Env {
    env_with(pool, fast()).await
}

/// То же с заданными настройками сценариев юзербота.
pub async fn env_with(pool: PgPool, flow: FlowConfig) -> Env {
    let world = SimWorld::new();
    let ub = world.add_account("exch_ub");
    let client = world.add_account("client");
    let mut tx = pool.begin().await.unwrap();
    let ub_id = storage::wallets::ensure_userbot(&mut tx, "ub-1", "+00******01")
        .await
        .unwrap();
    for platform in [CB, XR] {
        storage::wallets::ensure_wallet(&mut tx, platform, ub_id)
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();

    let engine = Engine::new(
        pool.clone(),
        EngineConfig {
            unavailable_backoff: Duration::ZERO,
            activation_retry_backoff: Duration::ZERO,
            ..EngineConfig::default()
        },
    );
    engine.ensure_ledger_accounts().await.unwrap();

    let mut tx = pool.begin().await.unwrap();
    for platform in [CB, XR] {
        let capital = ledger::capital_in(platform, Asset::Usdt, dec!(1000)).unwrap();
        storage::ledger::ensure_account(
            &mut tx,
            &AccountCode::equity(ledger::Equity::Capital, Asset::Usdt),
            Asset::Usdt,
            None,
        )
        .await
        .unwrap();
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
        world
            .set_balance(ub, platform, Asset::Usdt, dec!(1000))
            .unwrap();
    }
    for d in [Direction::CbToXrCheck, Direction::XrToCbCheck] {
        storage::directions::set_open(&mut tx, d, true)
            .await
            .unwrap();
    }
    storage::users::upsert(&mut tx, CLIENT, &storage::users::Profile::default())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let transport = Arc::new(world.transport(ub));
    let gate = Arc::new(AccountGate::new("ub-1"));
    let mut wallets = HashMap::new();
    let mut engine = engine;
    for platform in [CB, XR] {
        let w = Arc::new(
            UserbotWallet::new(platform, transport.clone(), gate.clone()).with_config(flow.clone()),
        );
        engine = engine.with_wallet(w.clone() as Arc<dyn WalletBot>);
        wallets.insert(platform, w);
    }
    Env {
        pool,
        world,
        ub,
        client,
        engine,
        wallets,
    }
}

impl Env {
    pub async fn submit(&self, url: &str) -> Submitted {
        let link = parsers::links::parse_url(url).unwrap();
        self.engine.submit_check(CLIENT, &link).await.unwrap()
    }

    pub async fn drain(&self) {
        for _ in 0..20 {
            if self.engine.process_due().await.unwrap() == 0 {
                return;
            }
        }
        panic!("operations keep coming");
    }

    pub async fn order(
        &self,
        id: i64,
    ) -> (storage::orders::OrderRow, storage::orders::OrderDetails) {
        let mut conn = self.pool.acquire().await.unwrap();
        (
            storage::orders::get(&mut conn, id, false).await.unwrap(),
            storage::orders::details(&mut conn, id).await.unwrap(),
        )
    }

    pub async fn ledger(&self, code: &str) -> Decimal {
        sqlx::query_scalar::<_, Option<Decimal>>(
            "SELECT SUM(e.amount) FROM ledger_entries e JOIN ledger_accounts a ON a.id = e.account_id WHERE a.code = $1",
        )
        .bind(code)
        .fetch_one(&self.pool)
        .await
        .unwrap()
        .unwrap_or_default()
    }

    /// Балансы аккаунта юзербота в симуляторе = балансы кошельков по леджеру; долг клиентам 0.
    pub async fn assert_books_match(&self) {
        assert_eq!(
            self.ledger("asset:cb:USDT").await,
            self.world.balance(self.ub, CB, Asset::Usdt),
            "CryptoBot"
        );
        assert_eq!(
            self.ledger("asset:xr:USDT").await,
            self.world.balance(self.ub, XR, Asset::Usdt),
            "xRocket"
        );
    }

    pub async fn ops(&self, order: i64) -> Vec<storage::operations::OperationRow> {
        let mut conn = self.pool.acquire().await.unwrap();
        storage::operations::for_order(&mut conn, order)
            .await
            .unwrap()
    }
}

pub fn accepted(s: Submitted) -> storage::orders::OrderRow {
    match s {
        Submitted::Accepted(order) => order,
        other => panic!("{other:?}"),
    }
}
