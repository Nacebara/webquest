//! Шаг 2.3 (находка Н-3): после перезагрузки сервера PostgreSQL поднимается позже exch. Старт
//! ждёт базу, а не выходит с ошибкой.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use sqlx::migrate::MigrateDatabase as _;
use tokio::net::{TcpListener, TcpStream};

/// `postgres://user:pass@host:port/db` → (адрес сервера, адрес с другим портом и базой).
fn rewrite(url: &str, port: u16, db: &str) -> (String, String) {
    let at = url.rfind('@').expect("DATABASE_URL has user@host");
    let rest = &url[at + 1..];
    let slash = rest.find('/').unwrap_or(rest.len());
    let server = rest[..slash].to_owned();
    let fwd = format!("{}127.0.0.1:{port}/{db}", &url[..=at]);
    (server, fwd)
}

#[tokio::test]
async fn start_waits_for_a_database_that_comes_up_late() {
    let url = std::env::var("DATABASE_URL").expect("tests need DATABASE_URL");
    // Свободный порт, на котором первые 2 секунды никто не слушает — «база ещё не поднялась».
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let db = format!("exch_startup_{}", std::process::id());
    let (server, late_url) = rewrite(&url, port, &db);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        loop {
            let (mut client, _) = listener.accept().await.unwrap();
            let server = server.clone();
            tokio::spawn(async move {
                let mut db = TcpStream::connect(server).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut client, &mut db).await;
            });
        }
    });

    let started = Instant::now();
    let pool = app::db::connect_and_migrate_waiting(&late_url, Duration::from_secs(60))
        .await
        .expect("the database came up within the limit");
    assert!(started.elapsed() >= Duration::from_secs(2));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM order_state_transitions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(n > 0, "migrations applied");
    pool.close().await;
    sqlx::Postgres::drop_database(&late_url).await.unwrap();
}

#[tokio::test]
async fn wrong_password_stops_the_start_at_once() {
    let url = std::env::var("DATABASE_URL").expect("tests need DATABASE_URL");
    let at = url.rfind('@').unwrap();
    let scheme_end = url.find("://").unwrap() + 3;
    let user = url[scheme_end..at].split(':').next().unwrap();
    let bad = format!(
        "{}{user}:definitely-wrong{}",
        &url[..scheme_end],
        &url[at..]
    );
    let started = Instant::now();
    let r = app::db::connect_and_migrate_waiting(&bad, Duration::from_secs(60)).await;
    assert!(r.is_err());
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "waited {:?} on a wrong password",
        started.elapsed()
    );
}
