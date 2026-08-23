//! Browser-assisted Agent pairing, activation and managed-instance deletion.

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use chrono::{Duration, Utc};
use sha2::{Digest, Sha256};
use sqlx_core::{query::query, row::Row};
use tower::ServiceExt;
use unionc::{
    config::{LocalConfig, Settings},
    http,
    infra::database,
    state::{AppState, LocalSession},
};
use uuid::Uuid;

mod common;

const SESSION: &str = "pairing-test-session";
const CSRF: &str = "pairing-test-csrf";

async fn app_with_database(url: String) -> (axum::Router, database::DbPool) {
    let mut settings = Settings::default();
    settings.database.url = url;
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");
    let state = AppState::new(
        settings,
        pool.clone(),
        "unused".to_string(),
        LocalConfig {
            application_version: env!("CARGO_PKG_VERSION").to_string(),
            admin_username: "admin".to_string(),
            admin_password_hash: "unused".to_string(),
        },
        unionc::system::ResourceMonitor::frozen(Default::default()),
    )
    .expect("capture test database identity");
    state.auth.sessions.write().await.insert(
        SESSION.to_string(),
        LocalSession {
            username: "admin".to_string(),
            expires_at: Utc::now() + Duration::minutes(10),
            csrf_token: CSRF.to_string(),
        },
    );
    (http::router(state), pool)
}

#[tokio::test]
async fn report_write_rechecks_host_existence_after_authentication() {
    let url =
        common::test_database_url("report_write_rechecks_host_existence_after_authentication");
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");

    let host_id_text = Uuid::new_v4().to_string();
    let host: unionc::monitoring::HostIdentity =
        serde_json::from_value(report_body(&host_id_text)["host"].clone())
            .expect("valid host fixture");
    let token_hash = hash(&secret());
    common::insert_active_monitoring_host(&pool, &host, &token_hash)
        .await
        .expect("insert host");

    // Model the first half of the HTTP handler: authentication completed just
    // before an administrator permanently deleted the instance.
    assert_eq!(
        unionc::monitoring::store::monitoring_host_for_token(&pool, &token_hash)
            .await
            .expect("authenticate credential"),
        unionc::monitoring::store::MonitoringTokenAuthentication::Active(host_id_text.clone())
    );
    assert!(
        unionc::monitoring::store::delete_monitored_host(&pool, &host_id_text)
            .await
            .expect("delete host")
    );

    let stale_in_flight: unionc::monitoring::AgentReport =
        serde_json::from_value(report_body(&host_id_text)).expect("valid report");
    let error = unionc::monitoring::store::store_authenticated_monitoring_report(
        &pool,
        &stale_in_flight,
        &token_hash,
    )
    .await
    .expect_err("the deleted in-flight host must not accept a report");
    assert!(matches!(
        error.downcast_ref::<unionc::monitoring::store::StoreReportError>(),
        Some(unionc::monitoring::store::StoreReportError::HostNotFound)
    ));
    let report_count: i64 = query("SELECT COUNT(*) AS count FROM agent_metric_reports")
        .fetch_one(&pool)
        .await
        .expect("count reports")
        .try_get("count")
        .unwrap();
    assert_eq!(report_count, 0);
}

#[tokio::test]
async fn inactive_invite_history_is_retained_then_reclaimed_in_bounded_batches() {
    const STALE_CANCELLED: i64 = 257;
    const STALE_EXPIRED: i64 = 257;
    const CLEANUP_BATCH_SIZE: i64 = 512;

    let url = common::test_database_url(
        "inactive_invite_history_is_retained_then_reclaimed_in_bounded_batches",
    );
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");

    let now = Utc::now();
    let created_at = database::to_epoch_micros(now - Duration::days(60));
    let cancelled_at = database::to_epoch_micros(now - Duration::days(32));
    let expired_at = database::to_epoch_micros(now - Duration::days(31));
    let recent_terminal_at = database::to_epoch_micros(now - Duration::days(29));
    let future_expiry = database::to_epoch_micros(now + Duration::minutes(15));
    let mut fixtures = database::begin_write(&pool).await.expect("begin fixtures");

    for index in 0..STALE_CANCELLED {
        query(
            r#"
            INSERT INTO agent_instance_invites(
                invite_id,instance_id,activation_code_hash,display_name,status,
                expires_at,created_at,cancelled_at
            ) VALUES(?1,?2,?3,'stale cancelled','cancelled',?4,?5,?6)
            "#,
        )
        .bind(Uuid::new_v4().to_string())
        .bind(Uuid::new_v4().to_string())
        .bind(format!("{:064x}", index + 1))
        .bind(expired_at)
        .bind(created_at)
        .bind(cancelled_at)
        .execute(fixtures.connection())
        .await
        .expect("insert stale cancelled invite");
    }
    for index in 0..STALE_EXPIRED {
        query(
            r#"
            INSERT INTO agent_instance_invites(
                invite_id,instance_id,activation_code_hash,display_name,expires_at,created_at
            ) VALUES(?1,?2,?3,'stale expired',?4,?5)
            "#,
        )
        .bind(Uuid::new_v4().to_string())
        .bind(Uuid::new_v4().to_string())
        .bind(format!("{:064x}", STALE_CANCELLED + index + 1))
        .bind(expired_at)
        .bind(created_at)
        .execute(fixtures.connection())
        .await
        .expect("insert stale expired invite");
    }

    let recent_cancelled_id = Uuid::new_v4().to_string();
    query(
        r#"
        INSERT INTO agent_instance_invites(
            invite_id,instance_id,activation_code_hash,display_name,status,
            expires_at,created_at,cancelled_at
        ) VALUES(?1,?2,?3,'recent cancelled','cancelled',?4,?5,?6)
        "#,
    )
    .bind(&recent_cancelled_id)
    .bind(Uuid::new_v4().to_string())
    .bind("e".repeat(64))
    .bind(future_expiry)
    .bind(created_at)
    .bind(recent_terminal_at)
    .execute(fixtures.connection())
    .await
    .expect("insert recent cancelled invite");

    let active_invite_id = Uuid::new_v4().to_string();
    query(
        r#"
        INSERT INTO agent_instance_invites(
            invite_id,instance_id,activation_code_hash,display_name,status,
            expires_at,created_at,activated_at
        ) VALUES(?1,?2,?3,'active invite','active',?4,?5,?6)
        "#,
    )
    .bind(&active_invite_id)
    .bind(Uuid::new_v4().to_string())
    .bind("d".repeat(64))
    .bind(expired_at)
    .bind(created_at)
    .bind(cancelled_at)
    .execute(fixtures.connection())
    .await
    .expect("insert active invite");
    fixtures.commit().await.expect("commit fixtures");

    let created = unionc::monitoring::store::create_agent_instance_invite(
        &pool,
        &Uuid::new_v4().to_string(),
        &Uuid::new_v4().to_string(),
        &"f".repeat(64),
        "new invite",
        now + Duration::minutes(15),
    )
    .await
    .expect("create invite after cleanup");
    assert!(matches!(
        created,
        unionc::monitoring::store::CreateInviteResult::Created(_)
    ));

    let stale_cancelled_remaining: i64 = query(
        "SELECT COUNT(*) AS count FROM agent_instance_invites \
         WHERE display_name='stale cancelled'",
    )
    .fetch_one(&pool)
    .await
    .expect("count stale cancelled invites")
    .try_get("count")
    .unwrap();
    let stale_expired_remaining: i64 = query(
        "SELECT COUNT(*) AS count FROM agent_instance_invites \
         WHERE display_name='stale expired'",
    )
    .fetch_one(&pool)
    .await
    .expect("count stale expired invites")
    .try_get("count")
    .unwrap();
    assert_eq!(stale_cancelled_remaining, 0);
    assert_eq!(
        stale_expired_remaining,
        STALE_CANCELLED + STALE_EXPIRED - CLEANUP_BATCH_SIZE,
        "one invitation creation must reclaim at most {CLEANUP_BATCH_SIZE} rows"
    );
    let recent_remaining: i64 =
        query("SELECT COUNT(*) AS count FROM agent_instance_invites WHERE invite_id=?1")
            .bind(recent_cancelled_id)
            .fetch_one(&pool)
            .await
            .expect("count retained recent invite")
            .try_get("count")
            .unwrap();
    assert_eq!(recent_remaining, 1, "recent terminal history was pruned");
    let active_remaining: i64 =
        query("SELECT COUNT(*) AS count FROM agent_instance_invites WHERE invite_id=?1")
            .bind(active_invite_id)
            .fetch_one(&pool)
            .await
            .expect("count retained active invite")
            .try_get("count")
            .unwrap();
    assert_eq!(active_remaining, 1, "active invitation was pruned");
}

#[tokio::test]
async fn anonymous_pairing_storage_is_bounded_and_reclaims_expired_rows() {
    const CLEANUP_BATCH_SIZE: i64 = 512;
    let url =
        common::test_database_url("anonymous_pairing_storage_is_bounded_and_reclaims_expired_rows");
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");

    let now = Utc::now();
    let now_micros = database::to_epoch_micros(now);
    let mut transaction = database::begin_write(&pool).await.expect("begin fixtures");
    let fixture_count =
        unionc::monitoring::store::MAX_PENDING_PAIRING_REQUESTS + CLEANUP_BATCH_SIZE + 1;
    for index in 0..fixture_count {
        let expired = index <= CLEANUP_BATCH_SIZE;
        let created_at = if expired {
            database::to_epoch_micros(now - Duration::minutes(20))
        } else {
            now_micros
        };
        let expires_at = if expired {
            database::to_epoch_micros(now - Duration::minutes(5))
        } else {
            database::to_epoch_micros(now + Duration::minutes(15))
        };
        query(
            r#"
            INSERT INTO agent_pairing_requests(
                request_id,requested_host_id,os,arch,agent_version,
                token_hash,polling_secret_hash,expires_at,created_at
            ) VALUES(?1,?2,'linux','x86_64','test',?3,?4,?5,?6)
            "#,
        )
        .bind(
            Uuid::from_u128(0x1000_0000_0000_0000_0000_0000_0000_0000 + index as u128).to_string(),
        )
        .bind(
            Uuid::from_u128(0x2000_0000_0000_0000_0000_0000_0000_0000 + index as u128).to_string(),
        )
        .bind(format!("{:064x}", index + 1))
        .bind(format!("{:064x}", fixture_count + index + 1))
        .bind(expires_at)
        .bind(created_at)
        .execute(transaction.connection())
        .await
        .expect("insert pending fixture");
    }
    transaction.commit().await.expect("commit fixtures");

    let candidate_token = secret();
    let candidate_poll = secret();
    let candidate: unionc::monitoring::AgentPairingRequest =
        serde_json::from_value(pairing_body(&candidate_token, &candidate_poll))
            .expect("valid pairing request");
    let first_result = unionc::monitoring::store::create_agent_pairing_request(
        &pool,
        &Uuid::new_v4().to_string(),
        &candidate,
        now + Duration::minutes(15),
    )
    .await
    .expect("first capacity result");
    assert!(matches!(
        first_result,
        unionc::monitoring::store::CreatePairingResult::AtCapacity
    ));
    let expired_remaining: i64 = query(
        "SELECT COUNT(*) AS count FROM agent_pairing_requests \
         WHERE status='pending' AND expires_at <= ?1",
    )
    .bind(now_micros)
    .fetch_one(&pool)
    .await
    .expect("count expired rows")
    .try_get("count")
    .unwrap();
    assert_eq!(
        expired_remaining, 1,
        "one transaction must reclaim at most {CLEANUP_BATCH_SIZE} expired rows"
    );
    let total_after_first: i64 = query("SELECT COUNT(*) AS count FROM agent_pairing_requests")
        .fetch_one(&pool)
        .await
        .expect("count rows after first cleanup")
        .try_get("count")
        .unwrap();
    assert_eq!(
        total_after_first,
        unionc::monitoring::store::MAX_PENDING_PAIRING_REQUESTS + 1,
        "AtCapacity must commit its bounded cleanup without inserting the candidate"
    );

    let second_result = unionc::monitoring::store::create_agent_pairing_request(
        &pool,
        &Uuid::new_v4().to_string(),
        &candidate,
        now + Duration::minutes(15),
    )
    .await
    .expect("capacity result");
    assert!(matches!(
        second_result,
        unionc::monitoring::store::CreatePairingResult::AtCapacity
    ));
    let total: i64 = query("SELECT COUNT(*) AS count FROM agent_pairing_requests")
        .fetch_one(&pool)
        .await
        .expect("count bounded rows")
        .try_get("count")
        .unwrap();
    assert_eq!(
        total,
        unionc::monitoring::store::MAX_PENDING_PAIRING_REQUESTS
    );
}

#[tokio::test]
async fn pairing_cleanup_reclaims_only_stale_denied_rows() {
    let url = common::test_database_url("pairing_cleanup_reclaims_only_stale_denied_rows");
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");

    let now = Utc::now();
    let stale_id = Uuid::new_v4().to_string();
    let recent_id = Uuid::new_v4().to_string();
    for (request_id, created_at) in [
        (
            &stale_id,
            database::to_epoch_micros(now - Duration::days(31)),
        ),
        (
            &recent_id,
            database::to_epoch_micros(now - Duration::days(29)),
        ),
    ] {
        query(
            r#"
            INSERT INTO agent_pairing_requests(
                request_id,requested_host_id,os,arch,agent_version,
                token_hash,polling_secret_hash,status,expires_at,created_at
            ) VALUES(?1,?2,'linux','x86_64','test',?3,?4,'denied',?5,?6)
            "#,
        )
        .bind(request_id)
        .bind(Uuid::new_v4().to_string())
        .bind(hash(&secret()))
        .bind(hash(&secret()))
        .bind(database::to_epoch_micros(now - Duration::minutes(1)))
        .bind(created_at)
        .execute(&pool)
        .await
        .expect("insert denied fixture");
    }

    let candidate_token = secret();
    let candidate_poll = secret();
    let candidate: unionc::monitoring::AgentPairingRequest =
        serde_json::from_value(pairing_body(&candidate_token, &candidate_poll))
            .expect("valid pairing request");
    let result = unionc::monitoring::store::create_agent_pairing_request(
        &pool,
        &Uuid::new_v4().to_string(),
        &candidate,
        now + Duration::minutes(15),
    )
    .await
    .expect("create pairing request");
    assert!(matches!(
        result,
        unionc::monitoring::store::CreatePairingResult::Ready(_)
    ));

    let stale_exists: i64 =
        query("SELECT EXISTS(SELECT 1 FROM agent_pairing_requests WHERE request_id=?1) AS found")
            .bind(&stale_id)
            .fetch_one(&pool)
            .await
            .expect("inspect stale denial")
            .try_get("found")
            .unwrap();
    let recent_exists: i64 =
        query("SELECT EXISTS(SELECT 1 FROM agent_pairing_requests WHERE request_id=?1) AS found")
            .bind(&recent_id)
            .fetch_one(&pool)
            .await
            .expect("inspect recent denial")
            .try_get("found")
            .unwrap();
    assert_eq!(
        stale_exists, 0,
        "a denial older than 30 days must be reclaimed"
    );
    assert_eq!(
        recent_exists, 1,
        "a recent denial remains available to the Agent"
    );
}

fn console(method: &str, path: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(path)
        .header("cookie", format!("session={SESSION}"))
        .header("x-csrf-token", CSRF)
}

async fn call_json(
    app: &axum::Router,
    request: axum::http::request::Builder,
    value: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    call_body(app, request, value, Some("application/json")).await
}

async fn call_body(
    app: &axum::Router,
    mut request: axum::http::request::Builder,
    value: serde_json::Value,
    content_type: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    if let Some(content_type) = content_type {
        request = request.header("content-type", content_type);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(value.to_string())).unwrap())
        .await
        .expect("request");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("response body");
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, body)
}

async fn call_empty(
    app: &axum::Router,
    request: axum::http::request::Builder,
) -> (StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .expect("request");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("response body");
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, body)
}

fn secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn pairing_body(token: &str, polling_secret: &str) -> serde_json::Value {
    serde_json::json!({
        "host": {
            "id": Uuid::new_v4(),
            "os": "linux",
            "os_version": "test",
            "kernel_version": "test-kernel",
            "arch": "x86_64",
            "agent_version": "0.3.4"
        },
        "token_hash": hash(token),
        "polling_secret_hash": hash(polling_secret)
    })
}

fn report_body(instance_id: &str) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "report_id": Uuid::new_v4(),
        "collected_at": Utc::now(),
        "host": {
            "id": instance_id,
            "os": "linux",
            "os_version": "test",
            "kernel_version": "test-kernel",
            "arch": "x86_64",
            "agent_version": "0.3.4"
        },
        "interval_seconds": 10.0,
        "system": {
            "uptime_seconds": 60,
            "cpu": {
                "usage_percent": 10.0,
                "logical_count": 4,
                "physical_count": 2,
                "per_core_percent": [10.0, 10.0, 10.0, 10.0]
            },
            "memory": {
                "total_bytes": 1000,
                "used_bytes": 500,
                "available_bytes": 500,
                "swap_total_bytes": 0,
                "swap_used_bytes": 0
            },
            "networks": [],
            "disks": [],
            "temperatures": [],
            "gpus": []
        },
        "capabilities": [],
        "agent": {"spool_pending_batches": 0, "collector_errors": 0}
    })
}

async fn create_invite(app: &axum::Router, display_name: &str) -> serde_json::Value {
    let body = serde_json::json!({"display_name": display_name});
    let (status, body) = call_json(
        app,
        console("POST", "/api/monitoring/agent-instances"),
        body,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body
}

async fn create_pairing(
    app: &axum::Router,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    call_json(app, Request::post("/api/agent/v2/pairing-requests"), body).await
}

async fn activate(
    app: &axum::Router,
    request_id: &str,
    activation_code: &str,
) -> (StatusCode, serde_json::Value) {
    call_json(
        app,
        Request::post("/api/agent/v2/activate"),
        serde_json::json!({
            "request_id": request_id,
            "activation_code": activation_code
        }),
    )
    .await
}

async fn poll(app: &axum::Router, request_id: &str, secret: &str) -> serde_json::Value {
    let (status, body) = call_empty(
        app,
        Request::post(format!(
            "/api/agent/v2/pairing-requests/{request_id}/status"
        ))
        .header("authorization", format!("Pairing {secret}")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn report(
    app: &axum::Router,
    instance_id: &str,
    token: &str,
) -> (StatusCode, serde_json::Value) {
    call_json(
        app,
        Request::post("/api/agent/v1/report").header("authorization", format!("Bearer {token}")),
        report_body(instance_id),
    )
    .await
}

#[tokio::test]
async fn current_pairing_contract_rejects_removed_fields_and_noncanonical_uuids() {
    let url = common::test_database_url("current_pairing_contract_is_strict");
    let (app, _pool) = app_with_database(url.to_string()).await;

    for content_type in [None, Some("text/plain")] {
        let (status, body) = call_body(
            &app,
            console("POST", "/api/monitoring/agent-instances"),
            serde_json::json!({"display_name": "strict"}),
            content_type,
        )
        .await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{body}");
        assert_eq!(body["code"], "unsupported_media_type");
    }

    let (status, _) = call_json(
        &app,
        console("POST", "/api/monitoring/agent-instances"),
        serde_json::json!({"display_name": "strict", "enrollment_code": "removed"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call_json(
        &app,
        console("POST", "/api/monitoring/agent-instances"),
        serde_json::json!({
            "display_name": "strict",
            "instance_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let token = secret();
    let polling = secret();
    let (status, body) = call_body(
        &app,
        Request::post("/api/agent/v2/pairing-requests"),
        pairing_body(&secret(), &secret()),
        Some("Application/JSON; Charset=\"UTF-8\""),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    for content_type in [None, Some("text/plain")] {
        let (status, body) = call_body(
            &app,
            Request::post("/api/agent/v2/pairing-requests"),
            pairing_body(&token, &polling),
            content_type,
        )
        .await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{body}");
        assert_eq!(body["code"], "unsupported_media_type");

        let (status, body) = call_body(
            &app,
            Request::post("/api/agent/v2/activate"),
            serde_json::json!({
                "request_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                "activation_code": "uci_current"
            }),
            content_type,
        )
        .await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{body}");
        assert_eq!(body["code"], "unsupported_media_type");
    }

    let mut pairing = pairing_body(&token, &polling);
    pairing["credential_kind"] = serde_json::json!("removed");
    assert_eq!(
        create_pairing(&app, pairing).await.0,
        StatusCode::BAD_REQUEST
    );

    let (status, _) = call_json(
        &app,
        Request::post("/api/agent/v2/activate"),
        serde_json::json!({
            "request_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "activation_code": "uci_current",
            "host_id": "removed"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    for noncanonical in [
        "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA",
        "aaaaaaaaaaaa4aaa8aaaaaaaaaaaaaaa",
    ] {
        let (status, _) = call_json(
            &app,
            console("POST", "/api/monitoring/agent-instances"),
            serde_json::json!({"instance_id": noncanonical}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let mut pairing = pairing_body(&token, &polling);
        pairing["host"]["id"] = serde_json::json!(noncanonical);
        assert_eq!(
            create_pairing(&app, pairing).await.0,
            StatusCode::BAD_REQUEST
        );

        assert_eq!(
            call_empty(
                &app,
                Request::get(format!("/api/agent/v2/pairing-requests/{noncanonical}")),
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            activate(&app, noncanonical, "uci_current").await.0,
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn pairing_is_atomic_replay_safe_and_creation_is_idempotent() {
    let url = common::test_database_url("pairing_is_atomic_replay_safe_and_creation_is_idempotent");
    let (app, pool) = app_with_database(url.to_string()).await;
    let invite = create_invite(&app, "paired instance").await;
    let instance_id = invite["instance_id"].as_str().unwrap();
    let activation_code = invite["activation_code"].as_str().unwrap();
    let token = secret();
    let polling_secret = secret();
    let pairing = pairing_body(&token, &polling_secret);

    let (first_status, first) = create_pairing(&app, pairing.clone()).await;
    assert_eq!(first_status, StatusCode::CREATED, "{first}");
    let request_id = first["request_id"].as_str().unwrap();
    assert_eq!(
        first["activation_url"],
        format!("/agent/activate/{request_id}")
    );

    // A response-lost retry reuses the request identified by polling_secret.
    let (retry_status, retry) = create_pairing(&app, pairing).await;
    assert_eq!(retry_status, StatusCode::OK, "{retry}");
    assert_eq!(retry["request_id"], request_id);

    // Two first attempts carrying the same locally persisted secrets may be
    // in flight together (service + interactive pair, or an HTTP retry). Both
    // must converge on one request rather than exposing an intermittent 409.
    let concurrent_token = secret();
    let concurrent_polling = secret();
    let concurrent_pairing = pairing_body(&concurrent_token, &concurrent_polling);
    let (left, right) = tokio::join!(
        create_pairing(&app, concurrent_pairing.clone()),
        create_pairing(&app, concurrent_pairing),
    );
    assert!(
        [StatusCode::CREATED, StatusCode::OK].contains(&left.0),
        "{}",
        left.1
    );
    assert!(
        [StatusCode::CREATED, StatusCode::OK].contains(&right.0),
        "{}",
        right.1
    );
    assert_eq!(left.1["request_id"], right.1["request_id"]);

    let (public_status, public) = call_empty(
        &app,
        Request::get(format!("/api/agent/v2/pairing-requests/{request_id}")),
    )
    .await;
    assert_eq!(public_status, StatusCode::OK, "{public}");
    assert!(public.get("name").is_none());
    assert_eq!(public["status"], "waiting");
    assert!(public.get("token_hash").is_none());
    assert!(public.get("polling_secret_hash").is_none());
    assert!(public.get("instance_id").is_none());

    assert_eq!(
        poll(&app, request_id, &polling_secret).await["status"],
        "waiting"
    );
    let (wrong_status, _) = call_empty(
        &app,
        Request::post(format!(
            "/api/agent/v2/pairing-requests/{request_id}/status"
        ))
        .header("authorization", format!("Pairing {}", secret())),
    )
    .await;
    assert_eq!(wrong_status, StatusCode::UNAUTHORIZED);

    let (wrong_code, _) = activate(&app, request_id, "uci_wrong").await;
    assert_eq!(wrong_code, StatusCode::UNAUTHORIZED);
    // Two browsers (or a browser retry racing the first response) may submit
    // the one-time code together. SQLite's write transaction must serialize
    // the read/check/update sequence: both calls converge on the same binding,
    // and neither can consume the invite for a different instance.
    let (activated, activation_retry) = tokio::join!(
        activate(&app, request_id, activation_code),
        activate(&app, request_id, activation_code),
    );
    assert_eq!(activated.0, StatusCode::OK, "{}", activated.1);
    assert_eq!(activation_retry.0, StatusCode::OK, "{}", activation_retry.1);
    assert_eq!(activated.1["instance_id"], instance_id);
    assert_eq!(activation_retry.1["instance_id"], instance_id);
    let active = poll(&app, request_id, &polling_secret).await;
    assert_eq!(active["status"], "active");
    assert_eq!(active["instance_id"], instance_id);
    assert_eq!(
        report(&app, instance_id, &token).await.0,
        StatusCode::ACCEPTED
    );

    // Pairing the Agent to another Server leaves older host reports at the head of the durable spool.
    // The dedicated machine code lets the Agent discard only those impossible reports without
    // treating an unrelated generic 403 as a stable UnionC authorization result.
    let previous_instance_id = Uuid::new_v4().to_string();
    let (mismatch_status, mismatch_body) = report(&app, &previous_instance_id, &token).await;
    assert_eq!(mismatch_status, StatusCode::FORBIDDEN, "{mismatch_body}");
    assert_eq!(mismatch_body["code"], "agent_host_mismatch");
    assert_eq!(
        report(&app, instance_id, &token).await.0,
        StatusCode::ACCEPTED,
        "the identity mismatch must not invalidate the current credential"
    );

    let (unauthenticated_status, _) = call_body(
        &app,
        Request::post("/api/agent/v1/report")
            .header("authorization", format!("Bearer {}", secret())),
        report_body(instance_id),
        Some("text/plain"),
    )
    .await;
    assert_eq!(
        unauthenticated_status,
        StatusCode::UNAUTHORIZED,
        "report authentication must run before media-type validation"
    );
    for content_type in [None, Some("text/plain")] {
        let (status, body) = call_body(
            &app,
            Request::post("/api/agent/v1/report")
                .header("authorization", format!("Bearer {token}")),
            report_body(instance_id),
            content_type,
        )
        .await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{body}");
        assert_eq!(body["code"], "unsupported_media_type");
    }

    // A consumed code cannot bind a different request.
    let second_token = secret();
    let second_poll = secret();
    let (_, second) = create_pairing(&app, pairing_body(&second_token, &second_poll)).await;
    let second_request_id = second["request_id"].as_str().unwrap();
    let (replay_status, _) = activate(&app, second_request_id, activation_code).await;
    assert_eq!(replay_status, StatusCode::CONFLICT);

    query("DELETE FROM agent_pairing_requests WHERE request_id=?1")
        .bind(second_request_id)
        .execute(&pool)
        .await
        .expect("cleanup pending request");
    query("DELETE FROM agent_pairing_requests WHERE request_id=?1")
        .bind(left.1["request_id"].as_str().unwrap())
        .execute(&pool)
        .await
        .expect("cleanup concurrent pending request");
    let (deleted, _) = call_empty(
        &app,
        console(
            "DELETE",
            &format!("/api/monitoring/managed-instances/{instance_id}"),
        ),
    )
    .await;
    assert_eq!(deleted, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn deleting_an_old_instance_preserves_an_activated_new_pairing() {
    let url =
        common::test_database_url("deleting_an_old_instance_preserves_an_activated_new_pairing");
    let (app, _pool) = app_with_database(url.to_string()).await;

    let old_invite = create_invite(&app, "old instance").await;
    let old_instance_id = old_invite["instance_id"].as_str().unwrap();
    let old_token = secret();
    let old_polling_secret = secret();
    let (_, old_pairing) =
        create_pairing(&app, pairing_body(&old_token, &old_polling_secret)).await;
    assert_eq!(
        activate(
            &app,
            old_pairing["request_id"].as_str().unwrap(),
            old_invite["activation_code"].as_str().unwrap(),
        )
        .await
        .0,
        StatusCode::OK
    );

    // A subsequent pairing request carries the Agent's old durable host id, while
    // activation deliberately binds it to the newly allocated invite instance.
    let new_invite = create_invite(&app, "new instance").await;
    let new_instance_id = new_invite["instance_id"].as_str().unwrap();
    let new_token = secret();
    let new_polling_secret = secret();
    let mut new_pairing_body = pairing_body(&new_token, &new_polling_secret);
    new_pairing_body["host"]["id"] = serde_json::json!(old_instance_id);
    let (_, new_pairing) = create_pairing(&app, new_pairing_body).await;
    let new_request_id = new_pairing["request_id"].as_str().unwrap();
    assert_eq!(
        activate(
            &app,
            new_request_id,
            new_invite["activation_code"].as_str().unwrap(),
        )
        .await
        .0,
        StatusCode::OK
    );

    // Model deletion after the activation transaction committed but before the
    // Agent received its status response. Polling must still recover the binding.
    assert_eq!(
        call_empty(
            &app,
            console(
                "DELETE",
                &format!("/api/monitoring/managed-instances/{old_instance_id}"),
            ),
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let recovered = poll(&app, new_request_id, &new_polling_secret).await;
    assert_eq!(recovered["status"], "active");
    assert_eq!(recovered["instance_id"], new_instance_id);
    assert_eq!(
        report(&app, new_instance_id, &new_token).await.0,
        StatusCode::ACCEPTED
    );
}

#[tokio::test]
async fn expired_pairing_and_invite_are_never_activated() {
    let url = common::test_database_url("expired_pairing_and_invite_are_never_activated");
    let (app, pool) = app_with_database(url.to_string()).await;
    let invite = create_invite(&app, "expiring instance").await;
    let token = secret();
    let polling_secret = secret();
    let (_, pairing) = create_pairing(&app, pairing_body(&token, &polling_secret)).await;
    let request_id = pairing["request_id"].as_str().unwrap();
    let invite_id = invite["request_id"].as_str().unwrap();

    let now = Utc::now();
    query("UPDATE agent_pairing_requests SET created_at=?2, expires_at=?3 WHERE request_id=?1")
        .bind(request_id)
        .bind(database::to_epoch_micros(now - Duration::minutes(20)))
        .bind(database::to_epoch_micros(now - Duration::minutes(10)))
        .execute(&pool)
        .await
        .expect("expire pairing");
    query("UPDATE agent_instance_invites SET created_at=?2, expires_at=?3 WHERE invite_id=?1")
        .bind(invite_id)
        .bind(database::to_epoch_micros(now - Duration::minutes(20)))
        .bind(database::to_epoch_micros(now - Duration::minutes(10)))
        .execute(&pool)
        .await
        .expect("expire invite");

    assert_eq!(
        poll(&app, request_id, &polling_secret).await["status"],
        "expired"
    );
    let (public_status, public) = call_empty(
        &app,
        Request::get(format!("/api/agent/v2/pairing-requests/{request_id}")),
    )
    .await;
    assert_eq!(public_status, StatusCode::OK);
    assert_eq!(public["status"], "expired");
    let (activation_status, _) = activate(
        &app,
        request_id,
        invite["activation_code"].as_str().unwrap(),
    )
    .await;
    assert_eq!(activation_status, StatusCode::GONE);

    let (list_status, list) =
        call_empty(&app, console("GET", "/api/monitoring/agent-instances")).await;
    assert_eq!(list_status, StatusCode::OK);
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|row| { row["request_id"] == invite_id && row["status"] == "expired" })
    );
    let (cancelled, _) = call_empty(
        &app,
        console(
            "DELETE",
            &format!("/api/monitoring/agent-instances/{invite_id}"),
        ),
    )
    .await;
    assert_eq!(cancelled, StatusCode::NO_CONTENT);

    query("DELETE FROM agent_pairing_requests WHERE request_id=?1")
        .bind(request_id)
        .execute(&pool)
        .await
        .expect("cleanup pairing");
    query("DELETE FROM agent_instance_invites WHERE invite_id=?1")
        .bind(invite_id)
        .execute(&pool)
        .await
        .expect("cleanup invite");
}

#[tokio::test]
async fn administrators_can_update_remark_then_permanently_delete_an_instance() {
    let url = common::test_database_url("administrators_can_update_remark_then_delete_an_instance");
    let (app, pool) = app_with_database(url.to_string()).await;
    let invite = create_invite(&app, "managed instance").await;
    let instance_id = invite["instance_id"].as_str().unwrap();
    let token = secret();
    let polling_secret = secret();
    let (_, pairing) = create_pairing(&app, pairing_body(&token, &polling_secret)).await;
    let request_id = pairing["request_id"].as_str().unwrap();
    assert_eq!(
        activate(
            &app,
            request_id,
            invite["activation_code"].as_str().unwrap(),
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        report(&app, instance_id, &token).await.0,
        StatusCode::ACCEPTED
    );

    let (read_resource_patch, _) = call_json(
        &app,
        console("PATCH", &format!("/api/monitoring/hosts/{instance_id}")),
        serde_json::json!({"remark": "must not be accepted here"}),
    )
    .await;
    assert_eq!(read_resource_patch, StatusCode::METHOD_NOT_ALLOWED);
    let (read_resource_delete, _) = call_empty(
        &app,
        console("DELETE", &format!("/api/monitoring/hosts/{instance_id}")),
    )
    .await;
    assert_eq!(read_resource_delete, StatusCode::METHOD_NOT_ALLOWED);
    let (removed_revoke_endpoint, _) = call_empty(
        &app,
        console(
            "POST",
            &format!("/api/monitoring/hosts/{instance_id}/revoke"),
        ),
    )
    .await;
    assert_eq!(removed_revoke_endpoint, StatusCode::NOT_FOUND);

    let (invalid_name, _) = call_json(
        &app,
        console(
            "PATCH",
            &format!("/api/monitoring/managed-instances/{instance_id}"),
        ),
        serde_json::json!({"remark": "  "}),
    )
    .await;
    assert_eq!(invalid_name, StatusCode::BAD_REQUEST);
    let (renamed, _) = call_json(
        &app,
        console(
            "PATCH",
            &format!("/api/monitoring/managed-instances/{instance_id}"),
        ),
        serde_json::json!({"remark": " 客厅工作站 "}),
    )
    .await;
    assert_eq!(renamed, StatusCode::NO_CONTENT);

    let mut later_report = report_body(instance_id);
    later_report["collected_at"] = serde_json::to_value(Utc::now() + Duration::seconds(1)).unwrap();
    assert_eq!(
        call_json(
            &app,
            Request::post("/api/agent/v1/report")
                .header("authorization", format!("Bearer {token}")),
            later_report,
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    let (detail_status, detail) = call_empty(
        &app,
        console("GET", &format!("/api/monitoring/hosts/{instance_id}")),
    )
    .await;
    assert_eq!(detail_status, StatusCode::OK, "{detail}");
    assert_eq!(detail["host"]["name"], "客厅工作站");

    let pending_token = secret();
    let pending_poll = secret();
    let mut pending_body = pairing_body(&pending_token, &pending_poll);
    pending_body["host"]["id"] = serde_json::Value::String(instance_id.to_string());
    assert_eq!(
        create_pairing(&app, pending_body).await.0,
        StatusCode::CREATED
    );

    let (deleted, _) = call_empty(
        &app,
        console(
            "DELETE",
            &format!("/api/monitoring/managed-instances/{instance_id}"),
        ),
    )
    .await;
    assert_eq!(deleted, StatusCode::NO_CONTENT);
    for table in [
        "monitored_hosts",
        "agent_metric_reports",
        "agent_credentials",
        "agent_pairing_requests",
        "agent_instance_invites",
    ] {
        let count: i64 = query(&format!(
            "SELECT COUNT(*) AS count FROM {table} WHERE {}=?1",
            match table {
                "monitored_hosts" => "host_id",
                "agent_metric_reports" | "agent_credentials" => "host_id",
                "agent_pairing_requests" => "requested_host_id",
                "agent_instance_invites" => "instance_id",
                _ => unreachable!(),
            }
        ))
        .bind(instance_id)
        .fetch_one(&pool)
        .await
        .expect("count deleted instance state")
        .try_get("count")
        .unwrap();
        assert_eq!(count, 0, "{table} retained deleted instance state");
    }
    let first_request_count: i64 =
        query("SELECT COUNT(*) AS count FROM agent_pairing_requests WHERE request_id=?1")
            .bind(request_id)
            .fetch_one(&pool)
            .await
            .expect("count activated pairing")
            .try_get("count")
            .unwrap();
    assert_eq!(first_request_count, 0);
    let invite_count: i64 =
        query("SELECT COUNT(*) AS count FROM agent_instance_invites WHERE invite_id=?1")
            .bind(invite["request_id"].as_str().unwrap())
            .fetch_one(&pool)
            .await
            .expect("count deleted invites")
            .try_get("count")
            .unwrap();
    assert_eq!(invite_count, 0);
    let audit_count: i64 = query(
        "SELECT COUNT(*) AS count FROM audit_logs WHERE action='monitoring.instance.delete' AND target=?1",
    )
    .bind(instance_id)
    .fetch_one(&pool)
    .await
    .expect("count deletion audit")
    .try_get("count")
    .unwrap();
    assert_eq!(audit_count, 1);
    assert_eq!(
        call_empty(
            &app,
            console("GET", &format!("/api/monitoring/hosts/{instance_id}")),
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        report(&app, instance_id, &token).await.0,
        StatusCode::UNAUTHORIZED
    );
}
