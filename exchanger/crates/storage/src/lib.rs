//! PostgreSQL-хранилище: подключение, миграции, заявки, леджер, аккаунты юзерботов и журнал
//! сообщений ботов кошельков (SPEC §10.7).
//!
//! Функции принимают `&mut PgConnection`, чтобы вызывающий код сам держал транзакцию:
//! переход заявки, операции outbox и проводки коммитятся вместе (SPEC §10.5.3).

use std::str::FromStr;
use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

pub mod admin;
pub mod alerts;
pub mod audit;
pub mod client;
pub mod client_prefs;
pub mod directions;
pub mod error;
pub mod holds;
pub mod invest;
pub mod ledger;
pub mod operations;
pub mod orders;
pub mod promo;
pub mod sessions;
pub mod settings;
pub mod staff;
pub mod transitions;
pub mod user_terms;
pub mod users;
pub mod wallet_messages;
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

/// Применить миграции. Миграции, которых эта версия exch не знает (база уже обновлена более
/// новой версией), не мешают запуску: откат на прежний exch.exe после обновления работает без
/// восстановления базы (веха 10). Новые миграции — только добавляют (схема — новыми миграциями,
/// правило 23); если какая-то убирает то, чем пользуется прежняя версия, откат — через копию.
pub async fn migrate(pool: &PgPool) -> Result<(), StorageError> {
    let mut m = sqlx::migrate!("../../migrations");
    m.set_ignore_missing(true);
    m.run(pool).await?;
    Ok(())
}

/// Миграции в базе, которых эта версия exch не знает.
pub async fn newer_migrations(pool: &PgPool) -> Result<Vec<i64>, StorageError> {
    let applied: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(pool)
            .await?;
    Ok(applied
        .into_iter()
        .filter(|v| !MIGRATOR.iter().any(|m| m.version == *v))
        .collect())
}
