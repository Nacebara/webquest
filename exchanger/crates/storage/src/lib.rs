//! PostgreSQL-хранилище: подключение, миграции, заявки, леджер (SPEC §10.7).
//!
//! Функции принимают `&mut PgConnection`, чтобы вызывающий код сам держал транзакцию:
//! переход заявки, операции outbox и проводки коммитятся вместе (SPEC §10.5.3).

use std::str::FromStr;
use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

pub mod error;
pub mod ledger;
pub mod orders;
pub mod transitions;
pub mod users;
pub mod wallets;

pub use error::StorageError;
pub use sqlx::PgPool;

/// Миграции встроены в бинарник и применяются при старте до открытия приёма заявок.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// Пул соединений с серверными таймаутами из SPEC §10.5.5.
pub async fn connect(database_url: &str) -> Result<PgPool, StorageError> {
    let options = PgConnectOptions::from_str(database_url)?
        .application_name("exch")
        .options([
            ("statement_timeout", "5s"),
            ("idle_in_transaction_session_timeout", "10s"),
        ]);
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(options)
        .await?;
    Ok(pool)
}

pub async fn migrate(pool: &PgPool) -> Result<(), StorageError> {
    MIGRATOR.run(pool).await?;
    Ok(())
}
