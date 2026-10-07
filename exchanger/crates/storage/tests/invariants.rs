//! Инварианты I1–I6 и охрана переходов на уровне БД — перенос schema_test.sql (SPEC §11.2).
//! Каждый тест пытается нарушить правило напрямую SQL-запросом и ждёт отказа БД.

#![allow(clippy::unwrap_used)]

mod common;

use common::{CLIENT, db_error_text, setup};
use sqlx::{PgPool, Row};

async fn insert_order(pool: &PgPool, state: &str, check: &str) -> i64 {
    sqlx::query(
        "INSERT INTO orders (user_id, direction, asset, intake_method, intake_platform, intake_start_param, state)
         VALUES ($1, 'cb_to_xr_check', 'USDT', 'check', 'cryptobot', $2, $3) RETURNING id",
    )
    .bind(CLIENT)
    .bind(check)
    .bind(state)
    .fetch_one(pool)
    .await
    .unwrap()
    .get(0)
}

/// Строка `operations` для тестов ограничений.
struct Op<'a> {
    order: i64,
    kind: &'a str,
    role: &'a str,
    via: &'a str,
    key: &'a str,
    start_param: Option<&'a str>,
    max_attempts: i32,
}

impl Op<'_> {
    async fn insert(&self, pool: &PgPool) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO operations (order_id, kind, money_role, via, platform, idempotency_key, start_param, max_attempts)
             VALUES ($1, $2, $3, $4, 'cryptobot', $5, $6, $7)",
        )
        .bind(self.order)
        .bind(self.kind)
        .bind(self.role)
        .bind(self.via)
        .bind(self.key)
        .bind(self.start_param)
        .bind(self.max_attempts)
        .execute(pool)
        .await
        .map(|_| ())
    }
}

async fn account(pool: &PgPool, code: &str) -> i32 {
    sqlx::query("SELECT id FROM ledger_accounts WHERE code = $1")
        .bind(code)
        .fetch_one(pool)
        .await
        .unwrap()
        .get(0)
}

/// I1: один чек — одна живая заявка.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn one_live_order_per_check(pool: PgPool) {
    setup(&pool).await;
    insert_order(&pool, "INTAKE_PENDING", "mc_abc").await;
    let err = sqlx::query(
        "INSERT INTO orders (user_id, direction, asset, intake_method, intake_platform, intake_start_param, state)
         VALUES ($1, 'cb_to_xr_check', 'USDT', 'check', 'cryptobot', 'mc_abc', 'INTAKE_PENDING')",
    )
    .bind(CLIENT)
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(
        db_error_text(&err).contains("orders_intake_param_live"),
        "{}",
        db_error_text(&err)
    );
}

/// I4: несбалансированная проводка отклоняется при коммите (отложенный триггер).
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn unbalanced_ledger_tx_is_rejected_on_commit(pool: PgPool) {
    setup(&pool).await;
    let xr = account(&pool, "asset:cb:USDT").await;
    let payable = account(&pool, "liability:payable:USDT").await;
    let mut tx = pool.begin().await.unwrap();
    let tx_id: i64 =
        sqlx::query("INSERT INTO ledger_transactions (kind) VALUES ('intake') RETURNING id")
            .fetch_one(&mut *tx)
            .await
            .unwrap()
            .get(0);
    sqlx::query("INSERT INTO ledger_entries (tx_id, account_id, asset, amount) VALUES ($1, $2, 'USDT', 100), ($1, $3, 'USDT', -97.5)")
        .bind(tx_id)
        .bind(xr)
        .bind(payable)
        .execute(&mut *tx)
        .await
        .unwrap();
    let err = tx.commit().await.unwrap_err();
    assert!(
        db_error_text(&err).contains("unbalanced"),
        "{}",
        db_error_text(&err)
    );
}

/// I4: счёт и строка проводки в одном активе.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn entry_asset_must_match_account(pool: PgPool) {
    setup(&pool).await;
    let xr = account(&pool, "asset:cb:USDT").await;
    let mut tx = pool.begin().await.unwrap();
    let tx_id: i64 =
        sqlx::query("INSERT INTO ledger_transactions (kind) VALUES ('intake') RETURNING id")
            .fetch_one(&mut *tx)
            .await
            .unwrap()
            .get(0);
    let err = sqlx::query(
        "INSERT INTO ledger_entries (tx_id, account_id, asset, amount) VALUES ($1, $2, 'TON', 1)",
    )
    .bind(tx_id)
    .bind(xr)
    .execute(&mut *tx)
    .await
    .unwrap_err();
    assert!(
        db_error_text(&err).contains("does not match account asset"),
        "{}",
        db_error_text(&err)
    );
}

/// I2: не больше одной расчётной операции (выплата / возврат / оплата счёта) на заявку.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn second_settle_operation_is_rejected(pool: PgPool) {
    setup(&pool).await;
    let order = insert_order(&pool, "INTAKE_PENDING", "mc_settle").await;
    Op {
        order,
        kind: "create_payout_check",
        role: "settle",
        via: "userbot",
        key: "ord-1-payout",
        start_param: None,
        max_attempts: 1,
    }
    .insert(&pool)
    .await
    .unwrap();
    let err = Op {
        order,
        kind: "create_refund_check",
        role: "settle",
        via: "userbot",
        key: "ord-1-refund",
        start_param: None,
        max_attempts: 1,
    }
    .insert(&pool)
    .await
    .unwrap_err();
    assert!(
        db_error_text(&err).contains("operations_one_settle_per_order"),
        "{}",
        db_error_text(&err)
    );

    // После окончательной ошибки прежней операции новая расчётная операция разрешена.
    sqlx::query("UPDATE operations SET status = 'failed' WHERE idempotency_key = 'ord-1-payout'")
        .execute(&pool)
        .await
        .unwrap();
    Op {
        order,
        kind: "create_refund_check",
        role: "settle",
        via: "userbot",
        key: "ord-1-refund",
        start_param: None,
        max_attempts: 1,
    }
    .insert(&pool)
    .await
    .unwrap();
}

/// I3: ключ идемпотентности уникален глобально.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn idempotency_key_is_globally_unique(pool: PgPool) {
    setup(&pool).await;
    let a = insert_order(&pool, "INTAKE_PENDING", "mc_a").await;
    let b = insert_order(&pool, "INTAKE_PENDING", "mc_b").await;
    Op {
        order: a,
        kind: "read_balance",
        role: "aux",
        via: "userbot",
        key: "same-key-1",
        start_param: None,
        max_attempts: 8,
    }
    .insert(&pool)
    .await
    .unwrap();
    let err = Op {
        order: b,
        kind: "read_balance",
        role: "aux",
        via: "userbot",
        key: "same-key-1",
        start_param: None,
        max_attempts: 8,
    }
    .insert(&pool)
    .await
    .unwrap_err();
    assert!(
        db_error_text(&err).contains("operations_idempotency_key_key"),
        "{}",
        db_error_text(&err)
    );
}

/// Действия юзербота, которые выводят деньги, — одна попытка: у кошелька нет ключа
/// идемпотентности, повтор вслепую может выплатить дважды (SPEC §10.5.4).
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn money_out_operations_must_be_single_attempt(pool: PgPool) {
    setup(&pool).await;
    let order = insert_order(&pool, "INTAKE_PENDING", "mc_pay").await;
    for (i, kind) in [
        "pay_invoice",
        "create_payout_check",
        "create_refund_check",
        "create_excess_check",
        "withdrawal",
    ]
    .into_iter()
    .enumerate()
    {
        let key = format!("ord-1-out-{i}");
        let err = Op {
            order,
            kind,
            role: "aux",
            via: "userbot",
            key: &key,
            start_param: None,
            max_attempts: 5,
        }
        .insert(&pool)
        .await
        .unwrap_err();
        assert!(
            db_error_text(&err).contains("operations_check1"),
            "{kind}: {}",
            db_error_text(&err)
        );
        Op {
            order,
            kind,
            role: "aux",
            via: "userbot",
            key: &key,
            start_param: None,
            max_attempts: 1,
        }
        .insert(&pool)
        .await
        .unwrap();
    }
}

/// Один чек никогда не активируется двумя операциями.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn check_is_activated_by_one_operation_only(pool: PgPool) {
    setup(&pool).await;
    let order = insert_order(&pool, "INTAKE_PENDING", "mc_once").await;
    Op {
        order,
        kind: "activate_check",
        role: "intake",
        via: "userbot",
        key: "ord-1-intake-1",
        start_param: Some("mc_zzz"),
        max_attempts: 8,
    }
    .insert(&pool)
    .await
    .unwrap();
    let err = Op {
        order,
        kind: "activate_check",
        role: "intake",
        via: "userbot",
        key: "ord-1-intake-2",
        start_param: Some("mc_zzz"),
        max_attempts: 8,
    }
    .insert(&pool)
    .await
    .unwrap_err();
    assert!(
        db_error_text(&err).contains("operations_check_once"),
        "{}",
        db_error_text(&err)
    );
}

/// I5: леджер и журналы только дописываются.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn ledger_and_journals_are_append_only(pool: PgPool) {
    setup(&pool).await;
    for sql in [
        "UPDATE ledger_entries SET amount = 1",
        "DELETE FROM ledger_entries",
        "UPDATE ledger_transactions SET memo = 'x'",
        "DELETE FROM ledger_transactions",
    ] {
        let err = sqlx::query(sql).execute(&pool).await.unwrap_err();
        assert!(
            db_error_text(&err).contains("append-only"),
            "{sql}: {}",
            db_error_text(&err)
        );
    }
}

/// Одна активная удержка на заявку.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn one_active_hold_per_order(pool: PgPool) {
    setup(&pool).await;
    let order = insert_order(&pool, "INTAKE_PENDING", "mc_hold").await;
    let wallet: i16 = sqlx::query("SELECT id FROM wallet_accounts WHERE label = 'xr'")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
    let hold = |amount: i32| {
        sqlx::query("INSERT INTO holds (order_id, wallet_account_id, asset, amount) VALUES ($1, $2, 'USDT', $3)")
            .bind(order)
            .bind(wallet)
            .bind(amount)
            .execute(&pool)
    };
    hold(300).await.unwrap();
    let err = hold(1).await.unwrap_err();
    assert!(
        db_error_text(&err).contains("holds_one_active_per_order"),
        "{}",
        db_error_text(&err)
    );
}

/// Триггер FSM не пускает недопустимый переход и не даёт выйти из терминального состояния.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn db_trigger_blocks_illegal_transitions(pool: PgPool) {
    setup(&pool).await;
    let order = insert_order(&pool, "INTAKE_PENDING", "mc_fsm").await;
    let err = sqlx::query("UPDATE orders SET state = 'PAYOUT_PENDING' WHERE id = $1")
        .bind(order)
        .execute(&pool)
        .await
        .unwrap_err();
    assert!(
        db_error_text(&err).contains("illegal order transition INTAKE_PENDING -> PAYOUT_PENDING"),
        "{}",
        db_error_text(&err)
    );
}

/// Заявку, по которой пришли деньги, нельзя закрыть как истёкшую или отменённую.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn received_money_blocks_silent_expiry(pool: PgPool) {
    setup(&pool).await;
    let id: i64 = sqlx::query(
        "INSERT INTO orders (user_id, direction, asset, intake_method, intake_platform, state, invoice_platform, invoice_start_param, quote_amount_in)
         VALUES ($1, 'pay_cb_invoice', 'USDT', 'check', 'xrocket', 'AWAITING_FUNDS', 'cryptobot', 'IVtest', 10.30) RETURNING id",
    )
    .bind(CLIENT)
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    sqlx::query("UPDATE orders SET intake_amount = 5 WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let err = sqlx::query("UPDATE orders SET state = 'EXPIRED', finished_at = now() WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap_err();
    assert!(
        db_error_text(&err).contains("orders_check3"),
        "{}",
        db_error_text(&err)
    );
}

/// I6: каждый сырой ответ бота кошелька хранится один раз.
#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn wallet_message_is_stored_once(pool: PgPool) {
    setup(&pool).await;
    let insert = || {
        sqlx::query(
            "INSERT INTO wallet_messages (userbot_id, bot_peer_id, message_id, edit_date, is_outgoing, text)
             VALUES (1, 777, 42, 0, false, 'You received 100 USDT')",
        )
        .execute(&pool)
    };
    insert().await.unwrap();
    let err = insert().await.unwrap_err();
    assert!(
        db_error_text(&err)
            .contains("wallet_messages_userbot_id_bot_peer_id_message_id_edit_date_key"),
        "{}",
        db_error_text(&err)
    );
}
