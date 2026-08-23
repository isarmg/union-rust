use sqlx_core::query::query;
use unionc::monitoring::HostIdentity;
use unionc::{config::Settings, infra::database};
use uuid::Uuid;

mod common;

fn identity(id: Uuid) -> HostIdentity {
    HostIdentity {
        id: id.to_string(),
        os: "linux".into(),
        os_version: Some("6.1".into()),
        kernel_version: Some("6.1".into()),
        arch: "x86_64".into(),
        agent_version: "0.3.4".into(),
    }
}

#[tokio::test]
async fn heartbeat_updates_do_not_move_hosts_between_offset_pages() {
    let url = common::test_database_url("stable_host_pagination");
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");

    let host_ids = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    for (index, host_id) in host_ids.iter().enumerate() {
        common::insert_active_monitoring_host(
            &pool,
            &identity(*host_id),
            &format!("{:064x}", index + 1),
        )
        .await
        .expect("insert host");
        query(
            "UPDATE monitored_hosts \
             SET registered_at=?2,last_seen_at=?3 \
             WHERE host_id=?1",
        )
        .bind(host_id.to_string())
        .bind((index + 1) as i64)
        .bind(300_i64 - index as i64 * 100)
        .execute(&pool)
        .await
        .expect("set deterministic timestamps");
    }

    let (first_page, total) = unionc::monitoring::store::list_monitored_hosts(&pool, 2, 0)
        .await
        .expect("read first page");
    assert_eq!(total, 3);
    assert_eq!(
        first_page
            .iter()
            .map(|host| host.identity.id.as_str())
            .collect::<Vec<_>>(),
        vec![host_ids[0].to_string(), host_ids[1].to_string()]
    );

    // A heartbeat arrives between page requests. It must not reorder rows that
    // the offset cursor has already passed, otherwise the next page duplicates
    // an old row and permanently skips the updated host.
    query("UPDATE monitored_hosts SET last_seen_at=400 WHERE host_id=?1")
        .bind(host_ids[2].to_string())
        .execute(&pool)
        .await
        .expect("record heartbeat");

    let (second_page, total) = unionc::monitoring::store::list_monitored_hosts(&pool, 2, 2)
        .await
        .expect("read second page");
    assert_eq!(total, 3);
    assert_eq!(
        second_page
            .iter()
            .map(|host| host.identity.id.as_str())
            .collect::<Vec<_>>(),
        vec![host_ids[2].to_string()]
    );
}
