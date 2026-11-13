//! Шаг 2.5 (Н-5): пинг наружу идёт, только когда движок и опрос Telegram живы.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use app::health::{Probe, run};
use chrono::Utc;
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

/// Пинг-сервис: считает запросы и отвечает `status`.
async fn service(status: u16) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/ping/abc", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            counter.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 2048];
            let _ = socket.read(&mut buf).await;
            let reply =
                format!("HTTP/1.1 {status} X\r\ncontent-length: 2\r\nconnection: close\r\n\r\nOK");
            let _ = socket.write_all(reply.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    (url, hits)
}

fn pinger(url: &str) -> Arc<botapi::Pinger> {
    Arc::new(botapi::Pinger::new(secrecy::SecretString::from(url.to_owned()), None, None).unwrap())
}

async fn beat_for(pool: PgPool, probes: Vec<Probe>, url: &str, how_long: Duration) {
    let cancel = CancellationToken::new();
    let task = tokio::spawn(run(
        pool,
        Some(pinger(url)),
        Arc::new(probes),
        Duration::from_millis(100),
        cancel.clone(),
    ));
    tokio::time::sleep(how_long).await;
    cancel.cancel();
    task.await.unwrap().unwrap();
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_working_bot_pings(pool: PgPool) {
    let (url, hits) = service(200).await;
    let alive = || Probe::new("engine", || Some(Utc::now()));
    beat_for(
        pool,
        vec![alive(), alive()],
        &url,
        Duration::from_millis(550),
    )
    .await;
    assert!(
        hits.load(Ordering::SeqCst) >= 3,
        "{}",
        hits.load(Ordering::SeqCst)
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_stuck_loop_stops_the_pings(pool: PgPool) {
    let (url, hits) = service(200).await;
    let engine = Probe::new("engine", || Some(Utc::now()));
    let stuck = Probe::new("telegram polling", || {
        Some(Utc::now() - chrono::Duration::minutes(10))
    });
    beat_for(pool, vec![engine, stuck], &url, Duration::from_millis(550)).await;
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "no pings while polling is stuck"
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn without_the_engine_there_are_no_pings(pool: PgPool) {
    let (url, hits) = service(200).await;
    beat_for(pool, Vec::new(), &url, Duration::from_millis(550)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_ping_reports_a_service_error_without_the_address() {
    let (url, _) = service(500).await;
    let err = pinger(&url).ping().await.unwrap_err().to_string();
    assert!(err.contains("500"), "{err}");
    assert!(!err.contains("/ping/abc"), "the address is a secret: {err}");
    let (ok, _) = service(200).await;
    pinger(&ok).ping().await.unwrap();
}
