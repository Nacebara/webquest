//! Резервные копии (веха 10) на живом PostgreSQL: копия снимается `pg_dump`, сразу
//! восстанавливается во временную базу и проверяется; испорченная копия не проходит проверку,
//! временная база после проверки не остаётся; итог виден в настройках и тревогах.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use app::backup::{self, BackupSettings, CHECK_DB};
use secrecy::SecretString;
use sqlx::PgPool;

/// Адрес тестовой базы `sqlx::test`: тот же сервер, что в `DATABASE_URL`.
async fn url_of(pool: &PgPool) -> SecretString {
    let base = std::env::var("DATABASE_URL").expect("tests need DATABASE_URL");
    let db: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(pool)
        .await
        .unwrap();
    let (head, query) = match base.split_once('?') {
        Some((h, q)) => (h.to_owned(), Some(q.to_owned())),
        None => (base.clone(), None),
    };
    let server = head.rsplit_once('/').unwrap().0;
    SecretString::from(match query {
        Some(q) => format!("{server}/{db}?{q}"),
        None => format!("{server}/{db}"),
    })
}

async fn check_db_exists(pool: &PgPool) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
        .bind(CHECK_DB)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_backup_is_restore_checked_and_a_broken_one_is_caught(pool: PgPool) {
    let url = url_of(&pool).await;
    {
        let mut conn = pool.acquire().await.unwrap();
        storage::staff::ensure_owner(&mut conn, 501, "Игнат")
            .await
            .unwrap();
        storage::audit::record(
            &mut conn,
            &storage::audit::NewAudit {
                staff_id: 501,
                action: "settings.flag",
                target_type: None,
                target_id: None,
                before: None,
                after: None,
                reason: None,
                double_confirmed: false,
            },
        )
        .await
        .unwrap();
    }
    let dir = std::env::temp_dir().join(format!("exch-backup-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let settings = BackupSettings {
        dir: Some(dir.clone()),
        at: "04:30".into(),
        keep: 2,
        pg_bin: None,
        key: Some(backup::BackupKey::from_bytes([7; 32])),
    };
    // Б-19.1: в копии — ссылки на незабранные чеки клиентов (деньги на предъявителя).
    let secret_link = "https://t.me/send?start=CQbackupSecret42";
    {
        let mut conn = pool.acquire().await.unwrap();
        storage::settings::set(
            &mut conn,
            "backup_probe",
            &serde_json::json!(secret_link),
            None,
        )
        .await
        .unwrap();
    }

    let o = backup::make(&pool, &url, &settings).await.unwrap();
    assert!(o.file.is_file() && o.bytes > 0);
    // Б-19.1 (проверка 19.11, решение владельца 20.11 — ключ в .env): в папке копий только шифр.
    let bytes = std::fs::read(&o.file).unwrap();
    assert!(!bytes.starts_with(b"PGDMP"), "копия не открытым pg_dump");
    assert!(
        !bytes
            .windows(secret_link.len())
            .any(|w| w == secret_link.as_bytes()),
        "ссылки на чеки в копии не читаются"
    );
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        1,
        "в папке копий ничего, кроме шифра"
    );
    // Чужой ключ — копию не открыть, и это сказано словами.
    let foreign = BackupSettings {
        key: Some(backup::BackupKey::from_bytes([8; 32])),
        ..settings.clone()
    };
    let err = backup::verify(&o.file, &url, &foreign, None)
        .await
        .expect_err("чужой ключ");
    assert!(err.to_string().contains("SESSION_KEY"), "{err:#}");
    assert!(!check_db_exists(&pool).await);
    assert!(
        o.checked
            .rows
            .iter()
            .any(|(t, n)| t == "admin_audit" && *n == 1),
        "{:?}",
        o.checked.rows
    );
    let latest = *storage::MIGRATOR.iter().map(|m| &m.version).max().unwrap();
    assert_eq!(o.checked.migration, latest);
    assert!(!check_db_exists(&pool).await, "временная база удалена");
    backup::record(&pool, &Ok(o.clone())).await.unwrap();
    let mut conn = pool.acquire().await.unwrap();
    let last = storage::settings::get(&mut conn, backup::LAST_KEY)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(last["checked"], true);
    drop(conn);

    // Старую копию можно проверить отдельно.
    backup::verify(&o.file, &url, &settings, None)
        .await
        .unwrap();

    // Испорченная копия: проверка падает, временной базы не остаётся, в пульте — тревога.
    let broken = dir.join("exch-2099-01-01-000000.dump");
    std::fs::write(&broken, b"PGDMP not really").unwrap();
    let err = backup::verify(&broken, &url, &settings, None)
        .await
        .expect_err("a broken dump must not pass");
    assert!(err.to_string().contains("pg_restore"), "{err:#}");
    assert!(!check_db_exists(&pool).await);
    backup::record(&pool, &Err(err)).await.unwrap();
    let mut conn = pool.acquire().await.unwrap();
    assert!(
        storage::alerts::open_by_key(&mut conn, "backup_failed")
            .await
            .unwrap()
            .is_some()
    );
    drop(conn);

    // Хранятся только `keep` последних.
    std::fs::remove_file(&broken).unwrap();
    for _ in 0..2 {
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        backup::make(&pool, &url, &settings).await.unwrap();
    }
    assert_eq!(backup::list(&dir).unwrap().len(), 2);

    // Восстановление: в базу с деньгами — отказ, в новую — проходит.
    {
        let mut conn = pool.acquire().await.unwrap();
        storage::users::upsert(&mut conn, 111, &storage::users::Profile::default())
            .await
            .unwrap();
        storage::orders::insert(
            &mut conn,
            &storage::orders::NewOrder {
                user_id: 111,
                direction: domain::Direction::CbToXrCheck,
                asset: domain::Asset::Usdt,
                intake_start_param: Some("CQrestore"),
                invoice_start_param: None,
            },
        )
        .await
        .unwrap();
    }
    let last = backup::list(&dir).unwrap().pop().unwrap();
    let refused = backup::restore(&last, &url, &settings)
        .await
        .expect_err("база с заявками не трогается");
    assert!(refused.to_string().contains("1 заявок"), "{refused:#}");
    let fresh = {
        let s = secrecy::ExposeSecret::expose_secret(&url).to_owned();
        let (head, _) = s.rsplit_once('/').unwrap();
        SecretString::from(format!("{head}/exch_restore_test_{}", std::process::id()))
    };
    let restored = backup::restore(&last, &fresh, &settings).await.unwrap();
    assert!(
        restored
            .rows
            .iter()
            .any(|(t, n)| t == "admin_audit" && *n == 1)
    );
    {
        use sqlx::migrate::MigrateDatabase as _;
        let fresh_url = secrecy::ExposeSecret::expose_secret(&fresh).to_owned();
        let p = storage::connect(&fresh_url).await.unwrap();
        let audit: i64 = sqlx::query_scalar("SELECT count(*) FROM admin_audit")
            .fetch_one(&p)
            .await
            .unwrap();
        assert_eq!(audit, 1);
        p.close().await;
        sqlx::Postgres::drop_database(&fresh_url).await.unwrap();
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
