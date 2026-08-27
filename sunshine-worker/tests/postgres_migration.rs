//! Real PostgreSQL proof for import verification and exact rollback.
//!
//! Set `SUNSHINE_TEST_DATABASE_URL` to opt in. The test uses unique host IDs,
//! restores an overwritten row, deletes a newly imported row and removes its
//! own audit/batch evidence after all assertions.

use unionc_sunshine_worker::{
    crypto::SecretBox,
    db,
    migration::{LogicalHost, import_hosts, rollback_batch, verify_batch},
    model::{HostPatchRequest, HostSaveRequest},
};

#[tokio::test]
async fn import_verify_and_rollback_are_exact_in_postgres() {
    let Ok(database_url) = std::env::var("SUNSHINE_TEST_DATABASE_URL") else {
        eprintln!("SUNSHINE_TEST_DATABASE_URL not set; skipping PostgreSQL integration proof");
        return;
    };
    let pool = db::connect(&database_url).await.unwrap();
    sqlx::query("CREATE SCHEMA IF NOT EXISTS sunshine")
        .execute(&pool)
        .await
        .unwrap();
    db::migrate(&pool).await.unwrap();
    let secrets = SecretBox::new("integration", [0x42; 32]).unwrap();
    let original = db::insert_host(
        &pool,
        &secrets,
        HostSaveRequest {
            name: "pre-import".into(),
            host: "old.example.test".into(),
            web_port: 47990,
            username: "old-admin".into(),
            password: Some("old-password".into()),
            verify_tls: true,
        },
        true,
        "integration-test",
    )
    .await
    .unwrap();
    let new_id = uuid::Uuid::new_v4().to_string();
    let imported = vec![
        LogicalHost {
            id: original.id.clone(),
            name: "from-sqlite".into(),
            host: "new.example.test".into(),
            web_port: 47991,
            username: "new-admin".into(),
            password: "new-password".into(),
            verify_tls: true,
            position: 41,
            created_at_micros: 111,
            updated_at_micros: 222,
        },
        LogicalHost {
            id: new_id.clone(),
            name: "new-from-sqlite".into(),
            host: "192.0.2.44".into(),
            web_port: 47992,
            username: "admin".into(),
            password: "second-password".into(),
            verify_tls: true,
            position: 42,
            created_at_micros: 333,
            updated_at_micros: 444,
        },
    ];
    let report = import_hosts(&pool, &secrets, imported.clone())
        .await
        .unwrap();
    assert!(report.verified);
    assert!(
        verify_batch(&pool, report.batch_id)
            .await
            .unwrap()
            .exact_match
    );
    let mapped = db::get_host(&pool, &secrets, &original.id).await.unwrap();
    assert_eq!(mapped.name, imported[0].name);
    assert_eq!(mapped.host, imported[0].host);
    assert_eq!(mapped.web_port, imported[0].web_port);
    assert_eq!(mapped.username, imported[0].username);
    assert_eq!(mapped.password, imported[0].password);
    assert_eq!(mapped.position, imported[0].position);
    assert_eq!(mapped.created_at_micros, imported[0].created_at_micros);
    assert_eq!(mapped.updated_at_micros, imported[0].updated_at_micros);

    let rollback = rollback_batch(&pool, report.batch_id).await.unwrap();
    assert!(rollback.exact_match);
    assert_eq!(
        db::get_host(&pool, &secrets, &original.id).await.unwrap(),
        original
    );
    assert!(db::get_host(&pool, &secrets, &new_id).await.is_err());

    db::delete_host(&pool, &original.id, "integration-test-cleanup")
        .await
        .unwrap();
    sqlx::query("DELETE FROM sunshine.audit_logs WHERE actor LIKE 'integration-test%'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM sunshine.import_batches WHERE batch_id=$1")
        .bind(report.batch_id)
        .execute(&pool)
        .await
        .unwrap();

    // Keep this import referenced so changes to public PATCH DTO compilation
    // remain covered by this external-crate integration target.
    let _ = HostPatchRequest::default();
}
