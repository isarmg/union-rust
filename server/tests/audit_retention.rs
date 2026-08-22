use sqlx_core::{query::query, row::Row};
use unionc::{config::Settings, infra::database};

mod common;

/// More than one internal batch proves pruning commits and continues while
/// still returning an exact aggregate count and preserving fresh rows.
#[tokio::test]
async fn audit_retention_prunes_multiple_batches_and_returns_exact_total() {
    let url = common::test_database_url("audit_retention_multiple_batches");
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings)
        .await
        .expect("connect test database");
    database::initialize_schema(&pool)
        .await
        .expect("initialize database schema");

    let old_created_at = database::to_epoch_micros(chrono::Utc::now() - chrono::Duration::days(8));
    query(
        r#"
        WITH digit(value) AS (
            VALUES (0),(1),(2),(3),(4),(5),(6),(7),(8),(9)
        )
        INSERT INTO audit_logs(action, target, created_at)
        SELECT 'test.retention', printf('old-%04d', row_number() OVER ()), ?
        FROM digit AS a
        CROSS JOIN digit AS b
        CROSS JOIN digit AS c
        CROSS JOIN digit AS d
        LIMIT 1005
        "#,
    )
    .bind(old_created_at)
    .execute(&pool)
    .await
    .expect("insert old audit backlog");

    for target in ["fresh-1", "fresh-2", "fresh-3"] {
        database::insert_audit(&pool, "test.retention", target, None)
            .await
            .expect("insert fresh audit row");
    }

    let removed = database::prune_audit_history(&pool, 7)
        .await
        .expect("prune audit history");
    assert_eq!(removed, 1_005);

    let old_remaining: i64 =
        query("SELECT COUNT(*) FROM audit_logs WHERE action='test.retention' AND created_at < ?")
            .bind(old_created_at + 1)
            .fetch_one(&pool)
            .await
            .expect("count old audit rows")
            .get(0);
    assert_eq!(old_remaining, 0);

    let fresh_remaining: i64 =
        query("SELECT COUNT(*) FROM audit_logs WHERE action='test.retention'")
            .fetch_one(&pool)
            .await
            .expect("count fresh audit rows")
            .get(0);
    assert_eq!(fresh_remaining, 3);
}
