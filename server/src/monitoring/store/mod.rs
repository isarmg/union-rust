//! Persistence for read-only host metric reports.

use chrono::{DateTime, Utc};
use sqlx_core::{query::query, row::Row};
use sqlx_sqlite::{SqliteConnection, SqliteRow};

use crate::monitoring::{
    AgentInstanceSummary, AgentPairingPublicSummary, AgentPairingRequest, AgentReport,
    AgentReportExt, PairingStatus,
};

use crate::infra::database::{self, DbPool};

mod host_types;
mod pairing_types;
mod report_types;
mod rows;

pub use host_types::*;
pub use pairing_types::*;
pub use report_types::*;
use rows::*;

// Persistence flows stay in this module scope so SQLite transaction helpers
// and row decoders remain private without widening the store API. The source
// is split by domain while preserving the original transaction boundaries.
include!("pairing.rs");
include!("reports.rs");
include!("hosts.rs");
include!("retention.rs");

#[cfg(test)]
mod activation_expiry_tests {
    use std::time::Duration as StdDuration;

    use sqlx_core::query::query;
    use tokio::sync::oneshot;

    use super::*;
    use crate::config::Settings;

    #[tokio::test]
    async fn activation_checks_expiry_after_waiting_for_the_writer_gate() {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let mut settings = Settings::default();
        settings.database.url = format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("unionc.db").display()
        );
        let pool = database::connect(&settings)
            .await
            .expect("connect database");
        database::initialize_schema(&pool)
            .await
            .expect("initialize schema");

        let request_id = uuid::Uuid::new_v4().to_string();
        let instance_id = uuid::Uuid::new_v4().to_string();
        let invite_id = uuid::Uuid::new_v4().to_string();
        let activation_code_hash = "c".repeat(64);
        let created_at = Utc::now();
        let expires_at = created_at + chrono::Duration::days(1);
        let mut fixtures = database::begin_write(&pool).await.expect("begin fixtures");
        query(
            r#"
            INSERT INTO agent_instance_invites(
                invite_id,instance_id,activation_code_hash,display_name,expires_at,created_at
            ) VALUES(?1,?2,?3,'expiring instance',?4,?5)
            "#,
        )
        .bind(&invite_id)
        .bind(&instance_id)
        .bind(&activation_code_hash)
        .bind(database::to_epoch_micros(expires_at))
        .bind(database::to_epoch_micros(created_at))
        .execute(fixtures.connection())
        .await
        .expect("insert invite");
        query(
            r#"
            INSERT INTO agent_pairing_requests(
                request_id,requested_host_id,name,os,arch,agent_version,
                token_hash,polling_secret_hash,expires_at,created_at
            ) VALUES(?1,?2,'expiring host','linux','x86_64','test',?3,?4,?5,?6)
            "#,
        )
        .bind(&request_id)
        .bind(&instance_id)
        .bind("a".repeat(64))
        .bind("b".repeat(64))
        .bind(database::to_epoch_micros(expires_at))
        .bind(database::to_epoch_micros(created_at))
        .execute(fixtures.connection())
        .await
        .expect("insert pairing request");
        fixtures.commit().await.expect("commit fixtures");

        let blocker = database::begin_write(&pool)
            .await
            .expect("hold writer gate");
        let (started_tx, started_rx) = oneshot::channel();
        let (clock_tx, mut clock_rx) = oneshot::channel();
        let worker_pool = pool.clone();
        let worker_request_id = request_id.clone();
        let worker_activation_hash = activation_code_hash.clone();
        let expired_now = expires_at + chrono::Duration::microseconds(1);
        let activation = tokio::spawn(async move {
            started_tx.send(()).expect("signal activation start");
            activate_agent_pairing_with_clock(
                &worker_pool,
                &worker_request_id,
                &worker_activation_hash,
                move || {
                    let _ = clock_tx.send(());
                    expired_now
                },
            )
            .await
        });
        started_rx.await.expect("activation task started");

        assert!(
            tokio::time::timeout(StdDuration::from_secs(1), &mut clock_rx)
                .await
                .is_err(),
            "the activation clock was read before the writer gate was acquired"
        );
        blocker.rollback().await.expect("release writer gate");
        tokio::time::timeout(StdDuration::from_secs(2), clock_rx)
            .await
            .expect("activation clock was not read after releasing the gate")
            .expect("activation clock sender was dropped");
        let result = activation
            .await
            .expect("activation task did not panic")
            .expect("activation query succeeds");
        assert_eq!(result, ActivatePairingResult::Expired);

        let host_count: i64 = query("SELECT COUNT(*) FROM monitored_hosts")
            .fetch_one(&pool)
            .await
            .expect("count monitored hosts")
            .try_get(0)
            .expect("decode monitored host count");
        assert_eq!(host_count, 0, "expired activation created a host");
        pool.close().await;
    }
}

#[cfg(test)]
mod history_query_plan_tests {
    use sqlx_core::{query::query, row::Row};

    use super::*;

    async fn explain_range(pool: &DbPool, has_from: bool, has_to: bool) -> Vec<String> {
        let sql = format!("EXPLAIN QUERY PLAN {}", history_query_sql(has_from, has_to));
        let statement = query(&sql).bind(uuid::Uuid::new_v4().to_string());
        let statement = match (has_from, has_to) {
            (false, false) => statement.bind(100_i64),
            (true, false) => statement.bind(10_i64).bind(100_i64),
            (false, true) => statement.bind(20_i64).bind(100_i64),
            (true, true) => statement.bind(10_i64).bind(20_i64).bind(100_i64),
        };
        statement
            .fetch_all(pool)
            .await
            .expect("explain production history query")
            .iter()
            .map(|row| row.try_get::<String, _>("detail").unwrap_or_default())
            .collect()
    }

    #[tokio::test]
    async fn every_history_range_shape_uses_the_compound_index_range() {
        let pool = database::in_memory_pool().expect("in-memory test pool");
        database::initialize_schema(&pool)
            .await
            .expect("initialize history plan schema");

        for (label, has_from, has_to) in [
            ("unbounded", false, false),
            ("from only", true, false),
            ("to only", false, true),
            ("both", true, true),
        ] {
            let plan = explain_range(&pool, has_from, has_to).await;
            let index_detail = plan
                .iter()
                .find(|detail| {
                    detail.contains("SEARCH agent_metric_reports")
                        && detail.contains("idx_agent_metric_reports_host_collected_at")
                })
                .unwrap_or_else(|| {
                    panic!("{label} query did not use the history index: {plan:#?}")
                });
            let compact = index_detail.replace(' ', "");
            assert_eq!(
                compact.contains("collected_at>?"),
                has_from,
                "{label} query has the wrong lower index bound: {index_detail}"
            );
            assert_eq!(
                compact.contains("collected_at<?"),
                has_to,
                "{label} query has the wrong upper index bound: {index_detail}"
            );
        }
    }
}
