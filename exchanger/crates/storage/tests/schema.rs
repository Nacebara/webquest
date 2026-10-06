//! Схема: файл миграции = schema.sql; таблица переходов в БД = domain::fsm::ALLOWED.

use std::collections::BTreeSet;
use std::path::PathBuf;

use sqlx::PgPool;

fn repo_file(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn migration_0001_is_identical_to_schema_sql() {
    assert!(
        repo_file("migrations/0001_init.sql") == repo_file("schema.sql"),
        "migrations/0001_init.sql must be a byte-for-byte copy of schema.sql (SPEC §10.7)"
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn db_transition_table_equals_domain_fsm(pool: PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let db: BTreeSet<_> = storage::transitions::load_allowed(&mut conn)
        .await
        .unwrap()
        .into_iter()
        .collect();
    let code: BTreeSet<_> = domain::fsm::ALLOWED.into_iter().collect();
    assert_eq!(db.len(), 36);
    assert_eq!(
        db, code,
        "order_state_transitions and domain::fsm::ALLOWED diverged"
    );
}

#[sqlx::test(migrator = "storage::MIGRATOR")]
async fn seeded_directions_match_domain(pool: PgPool) {
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT code, kind, source_platform, target_platform FROM directions ORDER BY code",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), domain::Direction::ALL.len());
    for (code, kind, source, target) in rows {
        let d: domain::Direction = code.parse().unwrap();
        let expected_kind = match d.flow() {
            domain::Flow::CheckExchange => "check_exchange",
            domain::Flow::InvoicePayment => "invoice_payment",
        };
        assert_eq!(kind, expected_kind, "{code}");
        assert_eq!(source, d.source().db_name(), "{code}");
        assert_eq!(target, d.target().db_name(), "{code}");
    }
}
