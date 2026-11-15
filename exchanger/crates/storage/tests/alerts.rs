//! Тревоги (`storage::alerts`) на живом PostgreSQL: «Принял» не должен заводить тревогу заново,
//! даже когда нажат одновременно с очередной проверкой пульта (гонка, найдена 16.11).

#![allow(clippy::unwrap_used)]

use serde_json::json;
use sqlx::PgPool;
use storage::alerts::{self, Level, NewAlert, Topic};

fn review(key: &str) -> NewAlert<'_> {
    NewAlert {
        level: Level::Critical,
        topic: Topic::Errors,
        dedup_key: key,
        title: "manual_review",
        payload: json!({"id": 1}),
    }
}

/// Сколько тревог с ключом сейчас «живые» (не ушли).
async fn ongoing(pool: &PgPool, key: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM alerts WHERE dedup_key = $1 AND resolved_at IS NULL")
        .bind(key)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn an_ack_racing_the_check_never_reopens_the_alert(pool: PgPool) {
    sqlx::query("INSERT INTO staff (tg_user_id, role, display_name) VALUES (1, 'owner', 'owner')")
        .execute(&pool)
        .await
        .unwrap();
    for round in 0..40 {
        let key = format!("manual_review:{round}");
        let mut conn = pool.acquire().await.unwrap();
        assert!(
            alerts::ensure_condition(&mut conn, &review(&key))
                .await
                .unwrap()
        );
        let id: i64 = sqlx::query_scalar("SELECT id FROM alerts WHERE dedup_key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .unwrap();
        // «Принял» и проверка пульта — одновременно, на разных соединениях.
        let (p1, p2, k1, k2) = (pool.clone(), pool.clone(), key.clone(), key.clone());
        let ack = tokio::spawn(async move {
            let mut c = p1.acquire().await.unwrap();
            alerts::ack(&mut c, id, 1).await.unwrap()
        });
        let check = tokio::spawn(async move {
            let mut c = p2.acquire().await.unwrap();
            alerts::ensure_condition(&mut c, &review(&k2))
                .await
                .unwrap();
            alerts::raise_condition(&mut c, &review(&k1)).await.unwrap()
        });
        assert!(ack.await.unwrap(), "«Принял» прошёл");
        check.await.unwrap();
        assert_eq!(
            ongoing(&pool, &key).await,
            1,
            "раунд {round}: тревога завелась заново"
        );
        // После «Принял» очередная проверка не открывает новую.
        assert!(
            !alerts::ensure_condition(&mut conn, &review(&key))
                .await
                .unwrap()
        );
        assert_eq!(ongoing(&pool, &key).await, 1);
    }
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn a_resolved_alert_opens_again_when_the_cause_returns(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let key = "manual_review:7";
    assert!(
        alerts::ensure_condition(&mut conn, &review(key))
            .await
            .unwrap()
    );
    assert!(
        !alerts::ensure_condition(&mut conn, &review(key))
            .await
            .unwrap(),
        "уже открыта"
    );
    assert!(alerts::resolve(&mut conn, key).await.unwrap());
    assert!(
        alerts::ensure_condition(&mut conn, &review(key))
            .await
            .unwrap(),
        "причина вернулась"
    );
    assert_eq!(ongoing(&pool, key).await, 1);
}
