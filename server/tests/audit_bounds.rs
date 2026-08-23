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
}
