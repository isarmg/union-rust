use sqlx_core::{query::query, row::Row};
use unionc::{config::Settings, infra::database};

mod common;

async fn uninitialized_database(test_name: &str) -> (common::TestDatabaseUrl, database::DbPool) {
    let url = common::test_database_url(test_name);
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings)
        .await
        .expect("connect test database");
    (url, pool)
}

#[tokio::test]
async fn database_with_objects_but_no_current_metadata_is_rejected() {
    let (_url, pool) = uninitialized_database("reject_database_without_current_metadata").await;
    query("CREATE TABLE settings(key TEXT PRIMARY KEY) STRICT")
        .execute(&pool)
        .await
        .expect("create non-current table");

    let error = database::initialize_schema(&pool).await.unwrap_err();
    assert!(error.to_string().contains("not the current UnionC schema"));
    let metadata_count: i64 =
        query("SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name='schema_metadata'")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
    assert_eq!(metadata_count, 0, "rejection must not mutate the database");
}

#[tokio::test]
async fn database_with_only_a_view_is_not_treated_as_empty() {
    let (_url, pool) = uninitialized_database("reject_view_without_current_metadata").await;
    query("CREATE VIEW unexpected_view AS SELECT 1 AS value")
        .execute(&pool)
        .await
        .unwrap();

    let error = database::initialize_schema(&pool).await.unwrap_err();
    assert!(error.to_string().contains("not the current UnionC schema"));
    let metadata_count: i64 =
        query("SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name='schema_metadata'")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
    assert_eq!(metadata_count, 0, "rejection must not mutate the database");
}

#[tokio::test]
async fn current_metadata_does_not_hide_extra_schema_objects() {
    let (_url, pool) = uninitialized_database("reject_extra_schema_object").await;
    database::initialize_schema(&pool).await.unwrap();
    query("CREATE TABLE unexpected_table(value TEXT) STRICT")
        .execute(&pool)
        .await
        .unwrap();

    let error = database::initialize_schema(&pool).await.unwrap_err();
    assert!(error.to_string().contains("exact current"));
}

#[tokio::test]
async fn current_metadata_does_not_hide_extra_views_or_triggers() {
    for (name, statement) in [
        (
            "view",
            "CREATE VIEW unexpected_view AS SELECT action FROM audit_logs",
        ),
        (
            "trigger",
            "CREATE TRIGGER unexpected_trigger AFTER INSERT ON audit_logs BEGIN SELECT 1; END",
        ),
    ] {
        let (_url, pool) = uninitialized_database(&format!("reject_extra_{name}")).await;
        database::initialize_schema(&pool).await.unwrap();
        query(statement).execute(&pool).await.unwrap();

        let error = database::initialize_schema(&pool).await.unwrap_err();
        assert!(
            error.to_string().contains("exact current"),
            "{name}: {error}"
        );
    }
}

#[tokio::test]
async fn current_metadata_does_not_hide_altered_schema_objects() {
    let (_url, pool) = uninitialized_database("reject_altered_schema_object").await;
    database::initialize_schema(&pool).await.unwrap();
    query("DROP INDEX idx_audit_logs_created_at")
        .execute(&pool)
        .await
        .unwrap();

    let error = database::initialize_schema(&pool).await.unwrap_err();
    assert!(error.to_string().contains("exact current"));
}

#[tokio::test]
async fn database_from_another_application_version_is_rejected() {
    let (_url, pool) = uninitialized_database("reject_other_application_version").await;
    database::initialize_schema(&pool).await.unwrap();
    query("UPDATE schema_metadata SET application_version='0.3.1'")
        .execute(&pool)
        .await
        .unwrap();

    let error = database::initialize_schema(&pool).await.unwrap_err();
    assert!(error.to_string().contains("schema metadata mismatch"));
}

/// 每次使用独立的临时 SQLite 文件运行，不允许因外部数据库缺失而跳过。
#[tokio::test]
async fn current_schema_is_versioned_and_idempotent() {
    let url = common::test_database_url("current_schema_is_versioned_and_idempotent");
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings)
        .await
        .expect("connect test database");

    database::initialize_schema(&pool)
        .await
        .expect("first schema initialization");
    database::initialize_schema(&pool)
        .await
        .expect("second schema initialization");

    let row = query(
        "SELECT COUNT(*) AS count,MAX(application_version) AS application_version, \
                MAX(checksum) AS checksum FROM schema_metadata",
    )
    .fetch_one(&pool)
    .await
    .expect("read schema version");
    assert_eq!(
        row.get::<i64, _>("count"),
        1,
        "schema_metadata must contain exactly the current schema record"
    );
    assert_eq!(row.get::<Option<String>, _>("checksum").unwrap().len(), 64);
    assert_eq!(
        row.get::<Option<String>, _>("application_version")
            .as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );

    let baseline = Settings::default();
    let loaded = database::load_app_settings(&pool, &baseline)
        .await
        .expect("load settings");
    assert_eq!(loaded.server.port, baseline.server.port);
    assert_eq!(loaded.sunshine.hosts.len(), baseline.sunshine.hosts.len());
    let obsolete_settings_table: i64 =
        query("SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name='settings'")
            .fetch_one(&pool)
            .await
            .expect("inspect obsolete settings table")
            .get(0);
    assert_eq!(obsolete_settings_table, 0);

    let mut audit_tx = database::begin_write(&pool)
        .await
        .expect("begin audit transaction");
    database::insert_audit_in_transaction(audit_tx.connection(), "test.rollback", "database", None)
        .await
        .expect("insert transactional audit");
    audit_tx.rollback().await.expect("roll back audit");
    let rolled_back: i64 = query("SELECT COUNT(*) FROM audit_logs WHERE action='test.rollback'")
        .fetch_one(&pool)
        .await
        .expect("count rolled-back audits")
        .get(0);
    assert_eq!(rolled_back, 0, "审计必须与调用方事务一起回滚");

    database::insert_audit(&pool, "test.prune", "database", Some("old row"))
        .await
        .expect("insert audit");
    query("UPDATE audit_logs SET created_at=? WHERE action='test.prune'")
        .bind(database::to_epoch_micros(
            chrono::Utc::now() - chrono::Duration::days(8),
        ))
        .execute(&pool)
        .await
        .expect("age audit");
    assert_eq!(
        database::prune_audit_history(&pool, 7)
            .await
            .expect("prune audit"),
        1
    );

    let invalid_external_host = query(
        "INSERT INTO external_hosts(kind,host_id,address,config,secret) VALUES('sunshine','invalid-json','127.0.0.1','[]',NULL)",
    ).execute(&pool).await;
    assert!(
        invalid_external_host.is_err(),
        "external host config must be a JSON object"
    );

    let unsupported_kind = query(
        "INSERT INTO external_hosts(kind,host_id,address,config,secret) VALUES('unsupported','invalid-kind','127.0.0.1','{}',NULL)",
    ).execute(&pool).await;
    assert!(
        unsupported_kind.is_err(),
        "only Sunshine hosts are accepted"
    );

    let monitoring_tables = query(
        "SELECT COUNT(*) AS count FROM sqlite_schema \
         WHERE type='table' AND name IN ('monitored_hosts','agent_metric_reports')",
    )
    .fetch_one(&pool)
    .await
    .expect("read monitoring tables");
    assert_eq!(monitoring_tables.get::<i64, _>("count"), 2);

    let foreign_keys: i64 = query("PRAGMA foreign_keys")
        .fetch_one(&pool)
        .await
        .expect("read foreign_keys pragma")
        .get(0);
    assert_eq!(foreign_keys, 1, "每条池连接都必须执行外键约束");

    let journal_mode: String = query("PRAGMA journal_mode")
        .fetch_one(&pool)
        .await
        .expect("read journal mode")
        .get(0);
    assert_eq!(journal_mode, "wal", "文件数据库必须使用 WAL");

    let synchronous: i64 = query("PRAGMA synchronous")
        .fetch_one(&pool)
        .await
        .expect("read synchronous pragma")
        .get(0);
    assert_eq!(synchronous, 2, "关键凭据必须使用 synchronous=FULL");

    let violations = query("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .expect("foreign key check");
    assert!(violations.is_empty(), "全新 schema 不得有外键违规");
}
