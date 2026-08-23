use sqlx_core::query::query;
use unionc::{config::Settings, infra::database};

mod common;

#[tokio::test]
async fn audit_details_are_bounded_and_control_characters_are_neutralized() {
    let url = common::test_database_url("bounded_audit_details");
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");

    let detail = format!("line one\n{}", "界".repeat(600));
    database::insert_audit(&pool, "test.bounded", "audit", Some(&detail))
        .await
        .expect("insert audit");

    let page = database::list_audit_logs(&pool, None, 1)
        .await
        .expect("read audit");
    let stored = page.entries[0].detail.as_deref().expect("audit detail");
    assert_eq!(stored.chars().count(), 512);
    assert!(stored.ends_with('…'));
    assert!(!stored.chars().any(char::is_control));
    assert!(stored.starts_with("line one "));

    // Simulate a row written by an older binary before the insertion bound
    // existed. The read path must not materialize or return its complete text.
    query("INSERT INTO audit_logs(action,target,detail) VALUES('test.legacy','audit',?1)")
        .bind(format!("legacy\n{}", "界".repeat(5_000)))
        .execute(&pool)
        .await
        .expect("insert legacy audit row directly");
    let page = database::list_audit_logs(&pool, None, 1)
        .await
        .expect("read legacy audit");
    let legacy = page.entries[0].detail.as_deref().expect("legacy detail");
    assert_eq!(legacy.chars().count(), 512);
    assert!(legacy.ends_with('…'));
    assert!(!legacy.chars().any(char::is_control));
    assert!(legacy.starts_with("legacy "));
}
