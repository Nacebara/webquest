//! База PostgreSQL: создать, если её нет, подключиться, применить миграции; узнать состояние
//! для `exch check` без изменений.

use anyhow::Context as _;
use sqlx::migrate::MigrateDatabase as _;

/// Подключиться к базе `url` (создать её, если ещё нет) и применить миграции.
pub async fn connect_and_migrate(url: &str) -> anyhow::Result<storage::PgPool> {
    match sqlx::Postgres::database_exists(url).await {
        Ok(false) => {
            sqlx::Postgres::create_database(url)
                .await
                .context("cannot create the database (does the user have CREATEDB?)")?;
            tracing::info!("database created");
        }
        Ok(true) => {}
        // Сервер ещё не отвечает: повторить с начала, иначе при подъёме сервера между проверкой и
        // подключением база не будет создана (гонку нашёл тест шага 2.3).
        Err(e) if crate::startup::transient_db(&e) => {
            return Err(anyhow::Error::new(e).context("cannot reach PostgreSQL"));
        }
        Err(e) => tracing::debug!(error = %e, "cannot check whether the database exists"),
    }
    let pool = storage::connect(url)
        .await
        .context("cannot connect to PostgreSQL")?;
    storage::migrate(&pool).await.context("migrations failed")?;
    tracing::info!("migrations applied");
    Ok(pool)
}

/// [`connect_and_migrate`] для запуска без присмотра (шаг 2.3): база ещё не поднялась после
/// перезагрузки — ждать до `limit`; неверный пароль или адрес — сразу ошибка.
pub async fn connect_and_migrate_waiting(
    url: &str,
    limit: std::time::Duration,
) -> anyhow::Result<storage::PgPool> {
    crate::startup::retry(
        "PostgreSQL",
        limit,
        || connect_and_migrate(url),
        crate::startup::transient_db_chain,
    )
    .await
}

/// Состояние базы для проверки: ничего не создаёт и не меняет.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbState {
    /// Сервер не отвечает или не пускает: текст ошибки по-русски.
    Unreachable(String),
    /// Сервер есть, базы ещё нет — создастся при запуске.
    Missing,
    /// База есть: применено миграций и сколько ещё применится при запуске.
    Ready { applied: usize, pending: usize },
}

pub async fn state(url: &str) -> DbState {
    match sqlx::Postgres::database_exists(url).await {
        Ok(false) => return DbState::Missing,
        Ok(true) => {}
        Err(e) => return DbState::Unreachable(explain(&e.to_string())),
    }
    let pool = match storage::connect(url).await {
        Ok(p) => p,
        Err(e) => return DbState::Unreachable(explain(&e.to_string())),
    };
    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
        .fetch_one(&pool)
        .await
        .unwrap_or(0);
    let applied = usize::try_from(applied).unwrap_or(0);
    let total = storage::MIGRATOR.iter().count();
    DbState::Ready {
        applied,
        pending: total.saturating_sub(applied),
    }
}

/// Частые ошибки подключения — понятными словами.
pub fn explain(error: &str) -> String {
    let e = error.to_lowercase();
    if e.contains("password authentication failed") {
        "пароль пользователя PostgreSQL не подходит".to_owned()
    } else if e.contains("connection refused") || e.contains("os error 10061") {
        "PostgreSQL не запущен (Службы → postgresql-x64-… → Запустить)".to_owned()
    } else if e.contains("does not exist") && e.contains("role") {
        "такого пользователя PostgreSQL нет".to_owned()
    } else {
        format!(
            "не удалось подключиться: {}",
            error.chars().take(200).collect::<String>()
        )
    }
}

/// Строка подключения из пароля пользователя `postgres` на этом компьютере.
pub fn local_url(password: &str) -> String {
    format!(
        "postgres://postgres:{}@localhost:5432/exch",
        percent_encode(password)
    )
}

/// Символы пароля, которые нельзя писать в URL как есть, — в `%XX`.
fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_with_special_characters_are_encoded() {
        assert_eq!(
            local_url("Moi2pass77"),
            "postgres://postgres:Moi2pass77@localhost:5432/exch"
        );
        assert_eq!(
            local_url("p@ss:w/rd?#"),
            "postgres://postgres:p%40ss%3Aw%2Frd%3F%23@localhost:5432/exch"
        );
    }

    #[test]
    fn connection_errors_are_explained() {
        assert!(
            explain(
                "error returned from database: password authentication failed for user \"postgres\""
            )
            .contains("пароль")
        );
        assert!(explain("Connection refused (os error 111)").contains("не запущен"));
    }
}
