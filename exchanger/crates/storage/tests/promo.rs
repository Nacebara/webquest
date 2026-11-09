//! Журнал рассылки «🔥 Выгодно» (`storage::promo`, миграция 0002) на живом PostgreSQL: одна
//! рассылка за раз, курсор, клиент помечается до отправки, «не чаще раза в 3 часа», бан.

#![allow(clippy::unwrap_used)]

use sqlx::PgPool;
use storage::promo;

async fn users(pool: &PgPool, ids: &[i64]) {
    for id in ids {
        sqlx::query("INSERT INTO users (tg_user_id) VALUES ($1)")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn one_broadcast_at_a_time_and_every_client_once(pool: PgPool) {
    users(&pool, &[30, 10, 20]).await;
    let mut conn = pool.acquire().await.unwrap();
    let id = promo::start(&mut conn, "cb_to_xr_check")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        promo::start(&mut conn, "xr_to_cb_check").await.unwrap(),
        None,
        "вторая, пока идёт первая, не заводится"
    );
    let b = promo::running(&mut conn).await.unwrap().unwrap();
    assert_eq!(
        (b.id, b.total, b.direction.as_str()),
        (id, 3, "cb_to_xr_check")
    );

    // По возрастанию id; каждый помечен до отправки.
    let mut got = Vec::new();
    while let Some((user, fresh)) = promo::claim_next(&mut conn, id).await.unwrap() {
        assert!(fresh);
        got.push(user);
        promo::count(&mut conn, id, user != 20).await.unwrap();
    }
    assert_eq!(got, vec![10, 20, 30]);
    let marked: i64 =
        sqlx::query_scalar("SELECT count(*) FROM users WHERE promo_notified_at IS NOT NULL")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(marked, 3);
    promo::finish(&mut conn, id).await.unwrap();
    let b = promo::last(&mut conn).await.unwrap().unwrap();
    assert!(b.finished_at.is_some());
    assert_eq!((b.delivered, b.failed, b.skipped), (2, 1, 0));
    assert_eq!(promo::running(&mut conn).await.unwrap(), None);

    // Следующая рассылка — меньше чем через 3 часа: все пропущены, никому не пишем.
    let next = promo::start(&mut conn, "xr_to_cb_check")
        .await
        .unwrap()
        .unwrap();
    while let Some((_, fresh)) = promo::claim_next(&mut conn, next).await.unwrap() {
        assert!(!fresh, "писали меньше 3 часов назад");
    }
    let b = promo::running(&mut conn).await.unwrap().unwrap();
    assert_eq!((b.skipped, b.delivered), (3, 0));
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn cooldown_passes_after_three_hours_and_banned_are_skipped(pool: PgPool) {
    users(&pool, &[1, 2, 3]).await;
    sqlx::query("INSERT INTO staff (tg_user_id, role, display_name) VALUES (1, 'owner', 'owner')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO bans (user_id, reason, banned_by) VALUES (3, 'spam', 1)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET promo_notified_at = now() - interval '3 hours 1 minute' WHERE tg_user_id = 2")
        .execute(&pool)
        .await
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        promo::recipients(&mut conn).await.unwrap(),
        2,
        "без забаненного"
    );
    let id = promo::start(&mut conn, "cb_to_xr_check")
        .await
        .unwrap()
        .unwrap();
    let mut got = Vec::new();
    while let Some(next) = promo::claim_next(&mut conn, id).await.unwrap() {
        got.push(next);
    }
    assert_eq!(
        got,
        vec![(1, true), (2, true)],
        "3 часа прошли; забаненному — нет"
    );
}
