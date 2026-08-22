//! Browser-assisted Agent pairing, activation, revocation and re-pairing.

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
async fn report_write_rechecks_the_exact_credential_after_authentication() {
    let url = common::test_database_url(
        "report_write_rechecks_the_exact_credential_after_authentication",
    );
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");

    let host_id = Uuid::new_v4();
    let host_id_text = host_id.to_string();
    let host: unionc::monitoring::HostIdentity =
        serde_json::from_value(report_body(&host_id_text, "credential-race-host")["host"].clone())
            .expect("valid host fixture");
    let old_token = secret();
    let old_hash = hash(&old_token);
    common::insert_active_monitoring_host(&pool, &host, &old_hash)
        .await
        .expect("insert host");

    // Model the first half of the HTTP handler: this request authenticated
    // before a concurrent administrator-approved re-pair rotated the token.
    assert_eq!(
        unionc::monitoring::store::monitoring_host_for_token(&pool, &old_hash)
            .await
            .expect("authenticate old credential"),
        unionc::monitoring::store::MonitoringTokenAuthentication::Active(host_id_text.clone())
    );

    let new_hash = hash(&secret());
    let now = database::now_epoch_micros();
    let mut transaction = database::begin_write(&pool).await.expect("begin rotation");
    query("UPDATE agent_credentials SET revoked_at=?2 WHERE host_id=?1 AND revoked_at IS NULL")
        .bind(&host_id_text)
        .bind(now)
        .execute(transaction.connection())
        .await
        .expect("revoke old credential");
    query(
        "INSERT INTO agent_credentials(credential_id,host_id,token_hash,issued_at) \
         VALUES(?1,?2,?3,?4)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&host_id_text)
    .bind(&new_hash)
    .bind(now)
    .execute(transaction.connection())
    .await
    .expect("insert replacement credential");
    transaction.commit().await.expect("commit rotation");

    let stale_in_flight: unionc::monitoring::AgentReport =
        serde_json::from_value(report_body(&host_id_text, "credential-race-host"))
            .expect("valid report");
    let error = unionc::monitoring::store::store_authenticated_monitoring_report(
        &pool,
        &stale_in_flight,
        &old_hash,
    )
    .await
    .expect_err("the revoked in-flight credential must not write");
    assert!(matches!(
        error.downcast_ref::<unionc::monitoring::store::StoreReportError>(),
        Some(unionc::monitoring::store::StoreReportError::CredentialNotActive)
    ));
    let report_count: i64 = query("SELECT COUNT(*) AS count FROM agent_metric_reports")
        .fetch_one(&pool)
        .await
        .expect("count reports")
        .try_get("count")
        .unwrap();
    assert_eq!(report_count, 0);

    unionc::monitoring::store::store_authenticated_monitoring_report(
        &pool,
        &stale_in_flight,
        &new_hash,
    )
    .await
    .expect("the replacement credential remains valid");
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
                request_id,requested_host_id,name,os,arch,agent_version,
                token_hash,polling_secret_hash,expires_at,created_at
            ) VALUES(?1,?2,'bounded-host','linux','x86_64','test',?3,?4,?5,?6)
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
    let candidate: unionc::monitoring::AgentPairingRequest = serde_json::from_value(pairing_body(
        &candidate_token,
        &candidate_poll,
        "over-capacity",
    ))
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
                request_id,requested_host_id,name,os,arch,agent_version,
                token_hash,polling_secret_hash,status,expires_at,created_at
            ) VALUES(?1,?2,'denied-host','linux','x86_64','test',?3,?4,'denied',?5,?6)
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
    let candidate: unionc::monitoring::AgentPairingRequest = serde_json::from_value(pairing_body(
        &candidate_token,
        &candidate_poll,
        "cleanup-trigger",
    ))
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

fn pairing_body(token: &str, polling_secret: &str, name: &str) -> serde_json::Value {
    serde_json::json!({
        "host": {
            "id": Uuid::new_v4(),
            "name": name,
            "os": "linux",
            "os_version": "test",
            "kernel_version": "test-kernel",
            "arch": "x86_64",
            "agent_version": "0.3.2"
        },
        "token_hash": hash(token),
        "polling_secret_hash": hash(polling_secret)
    })
}

fn report_body(instance_id: &str, name: &str) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "report_id": Uuid::new_v4(),
        "collected_at": Utc::now(),
        "host": {
            "id": instance_id,
            "name": name,
            "os": "linux",
            "os_version": "test",
            "kernel_version": "test-kernel",
            "arch": "x86_64",
            "agent_version": "0.3.2"
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

async fn create_invite(
    app: &axum::Router,
    display_name: &str,
    instance_id: Option<&str>,
) -> serde_json::Value {
    let mut body = serde_json::json!({"display_name": display_name});
    if let Some(instance_id) = instance_id {
        body["instance_id"] = serde_json::Value::String(instance_id.to_string());
    }
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
        report_body(instance_id, "paired-host"),
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

    let token = secret();
    let polling = secret();
    let (status, body) = call_body(
        &app,
        Request::post("/api/agent/v2/pairing-requests"),
        pairing_body(&secret(), &secret(), "parameterized-json"),
        Some("Application/JSON; Charset=\"UTF-8\""),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    for content_type in [None, Some("text/plain")] {
        let (status, body) = call_body(
            &app,
            Request::post("/api/agent/v2/pairing-requests"),
            pairing_body(&token, &polling, "strict-host"),
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

    let mut pairing = pairing_body(&token, &polling, "strict-host");
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

        let mut pairing = pairing_body(&token, &polling, "strict-host");
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
    let invite = create_invite(&app, "paired instance", None).await;
    let instance_id = invite["instance_id"].as_str().unwrap();
    let activation_code = invite["activation_code"].as_str().unwrap();
    let token = secret();
    let polling_secret = secret();
    let pairing = pairing_body(&token, &polling_secret, "paired-host");

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
    let concurrent_pairing = pairing_body(
        &concurrent_token,
        &concurrent_polling,
        "concurrent-paired-host",
    );
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
    assert_eq!(public["name"], "paired-host");
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

    // Re-pairing to another server leaves older host reports at the head of the durable spool.
    // The dedicated machine code lets the Agent discard only those impossible reports without
    // treating an unrelated generic 403 as either a host mismatch or a revoked credential.
    let previous_instance_id = Uuid::new_v4().to_string();
    let (mismatch_status, mismatch_body) = report(&app, &previous_instance_id, &token).await;
    assert_eq!(mismatch_status, StatusCode::FORBIDDEN, "{mismatch_body}");
    assert_eq!(mismatch_body["code"], "agent_host_mismatch");
    assert_eq!(
        report(&app, instance_id, &token).await.0,
        StatusCode::ACCEPTED,
        "the identity mismatch must not revoke the current credential"
    );

    let (unauthenticated_status, _) = call_body(
        &app,
        Request::post("/api/agent/v1/report")
            .header("authorization", format!("Bearer {}", secret())),
        report_body(instance_id, "paired-host"),
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
            report_body(instance_id, "paired-host"),
            content_type,
        )
        .await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{body}");
        assert_eq!(body["code"], "unsupported_media_type");
    }

    // A consumed code cannot bind a different request.
    let second_token = secret();
    let second_poll = secret();
    let (_, second) = create_pairing(
        &app,
        pairing_body(&second_token, &second_poll, "other-host"),
    )
    .await;
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
            "POST",
            &format!("/api/monitoring/hosts/{instance_id}/revoke"),
        ),
    )
    .await;
    assert_eq!(deleted, StatusCode::NO_CONTENT);
    query("DELETE FROM monitored_hosts WHERE host_id=?1")
        .bind(instance_id)
        .execute(&pool)
        .await
        .expect("cleanup host tombstone");
    query("DELETE FROM agent_instance_invites WHERE invite_id=?1")
        .bind(invite["request_id"].as_str().unwrap())
        .execute(&pool)
        .await
        .expect("cleanup invite");
}

#[tokio::test]
async fn expired_pairing_and_invite_are_never_activated() {
    let url = common::test_database_url("expired_pairing_and_invite_are_never_activated");
    let (app, pool) = app_with_database(url.to_string()).await;
    let invite = create_invite(&app, "expiring instance", None).await;
    let token = secret();
    let polling_secret = secret();
    let (_, pairing) =
        create_pairing(&app, pairing_body(&token, &polling_secret, "expiring-host")).await;
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
async fn revoke_is_terminal_until_an_admin_re_pairs_the_same_instance() {
    let url =
        common::test_database_url("revoke_is_terminal_until_an_admin_re_pairs_the_same_instance");
    let (app, pool) = app_with_database(url.to_string()).await;
    let first_invite = create_invite(&app, "stable instance", None).await;
    let instance_id = first_invite["instance_id"].as_str().unwrap();
    let first_token = secret();
    let first_poll = secret();
    let (_, first_pairing) =
        create_pairing(&app, pairing_body(&first_token, &first_poll, "paired-host")).await;
    let first_request_id = first_pairing["request_id"].as_str().unwrap();
    assert_eq!(
        activate(
            &app,
            first_request_id,
            first_invite["activation_code"].as_str().unwrap()
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        {
            let mut old_report = report_body(instance_id, "old-generation-host");
            old_report["report_id"] = serde_json::Value::String(Uuid::new_v4().to_string());
            old_report["collected_at"] =
                serde_json::to_value(Utc::now() + Duration::minutes(4)).unwrap();
            old_report["capabilities"] = serde_json::json!([{
                "name": "old-generation-capability",
                "available": true,
                "source": "test",
                "error_kind": null,
                "message": null
            }]);
            call_json(
                &app,
                Request::post("/api/agent/v1/report")
                    .header("authorization", format!("Bearer {first_token}")),
                old_report,
            )
            .await
            .0
        },
        StatusCode::ACCEPTED,
        "the first credential generation must establish an intentionally future latest point"
    );

    // An invite issued before decommissioning is an outstanding capability to
    // reactivate this exact tombstone. Revocation must retire it (and a pending
    // request for the same id), otherwise the old activation code can undo the
    // later administrator action.
    let stale_invite = create_invite(&app, "stale re-pair", Some(instance_id)).await;
    let stale_token = secret();
    let stale_poll = secret();
    let mut stale_pairing_body = pairing_body(&stale_token, &stale_poll, "stale-host");
    stale_pairing_body["host"]["id"] = serde_json::Value::String(instance_id.to_string());
    let (stale_status, stale_pairing) = create_pairing(&app, stale_pairing_body).await;
    assert_eq!(stale_status, StatusCode::CREATED, "{stale_pairing}");
    let stale_request_id = stale_pairing["request_id"].as_str().unwrap();

    let (revoked, _) = call_empty(
        &app,
        console(
            "POST",
            &format!("/api/monitoring/hosts/{instance_id}/revoke"),
        ),
    )
    .await;
    assert_eq!(revoked, StatusCode::NO_CONTENT);
    assert_eq!(
        poll(&app, stale_request_id, &stale_poll).await["status"],
        "denied"
    );
    let (stale_activation, _) = activate(
        &app,
        stale_request_id,
        stale_invite["activation_code"].as_str().unwrap(),
    )
    .await;
    assert_eq!(stale_activation, StatusCode::CONFLICT);
    let (old_report_status, old_report_body) = report(&app, instance_id, &first_token).await;
    assert_eq!(old_report_status, StatusCode::FORBIDDEN);
    assert_eq!(old_report_body["code"], "agent_revoked");
    assert_eq!(
        poll(&app, first_request_id, &first_poll).await["status"],
        "denied"
    );
    assert_eq!(
        activate(
            &app,
            first_request_id,
            first_invite["activation_code"].as_str().unwrap()
        )
        .await
        .0,
        StatusCode::CONFLICT
    );

    let lifecycle: String = query("SELECT lifecycle_status FROM monitored_hosts WHERE host_id=?1")
        .bind(instance_id)
        .fetch_one(&pool)
        .await
        .expect("host tombstone")
        .try_get("lifecycle_status")
        .unwrap();
    assert_eq!(lifecycle, "revoked");

    // An explicit administrator invite is the only path that reactivates the
    // same instance, rotating credentials while preserving its report rows.
    let second_invite = create_invite(&app, "stable instance re-pair", Some(instance_id)).await;
    assert_eq!(second_invite["instance_id"], instance_id);
    let second_token = secret();
    let second_poll = secret();
    let (_, second_pairing) = create_pairing(
        &app,
        pairing_body(&second_token, &second_poll, "paired-host"),
    )
    .await;
    let second_request_id = second_pairing["request_id"].as_str().unwrap();
    let (second_activation, body) = activate(
        &app,
        second_request_id,
        second_invite["activation_code"].as_str().unwrap(),
    )
    .await;
    assert_eq!(second_activation, StatusCode::OK, "{body}");
    assert_eq!(body["instance_id"], instance_id);

    let reset = unionc::monitoring::store::get_monitored_host(&pool, instance_id)
        .await
        .expect("read re-paired host")
        .expect("re-paired host exists");
    assert!(reset.latest.is_none());
    assert!(reset.latest_collected_at.is_none());
    assert!(reset.latest_interval_seconds.is_none());
    assert!(reset.capabilities.is_empty());
    let retained_old_payload: Option<String> =
        query("SELECT payload FROM agent_metric_reports WHERE host_id=?1")
            .bind(instance_id)
            .fetch_one(&pool)
            .await
            .expect("retained old report")
            .try_get("payload")
            .unwrap();
    assert!(
        retained_old_payload.is_none(),
        "the previous generation must not retain a detail payload after re-pairing"
    );

    let new_report_id = Uuid::new_v4().to_string();
    let mut new_report = report_body(instance_id, "new-generation-host");
    new_report["report_id"] = serde_json::Value::String(new_report_id.clone());
    new_report["collected_at"] = serde_json::to_value(Utc::now()).unwrap();
    new_report["capabilities"] = serde_json::json!([{
        "name": "new-generation-capability",
        "available": true,
        "source": "test",
        "error_kind": null,
        "message": null
    }]);
    assert_eq!(
        call_json(
            &app,
            Request::post("/api/agent/v1/report")
                .header("authorization", format!("Bearer {second_token}")),
            new_report,
        )
        .await
        .0,
        StatusCode::ACCEPTED,
        "a new-generation report must become current despite the old future timestamp"
    );
    let current = unionc::monitoring::store::get_monitored_host(&pool, instance_id)
        .await
        .expect("read current host")
        .expect("current host exists");
    assert_eq!(current.identity.name, "new-generation-host");
    assert_eq!(current.capabilities.len(), 1);
    assert_eq!(current.capabilities[0].name, "new-generation-capability");
    assert_eq!(
        current
            .latest
            .as_ref()
            .map(|report| report.report_id.as_str()),
        Some(new_report_id.as_str())
    );
    assert_eq!(
        report(&app, instance_id, &first_token).await.0,
        // The instance is active again, but this superseded secret remains
        // invalid. 403 is reserved for a currently revoked host tombstone.
        StatusCode::UNAUTHORIZED
    );
    let report_count: i64 =
        query("SELECT COUNT(*) AS count FROM agent_metric_reports WHERE host_id=?1")
            .bind(instance_id)
            .fetch_one(&pool)
            .await
            .expect("report count")
            .try_get("count")
            .unwrap();
    assert_eq!(report_count, 2, "re-pair must preserve the first report");

    let (deleted, _) = call_empty(
        &app,
        console(
            "POST",
            &format!("/api/monitoring/hosts/{instance_id}/revoke"),
        ),
    )
    .await;
    assert_eq!(deleted, StatusCode::NO_CONTENT);
    query("DELETE FROM monitored_hosts WHERE host_id=?1")
        .bind(instance_id)
        .execute(&pool)
        .await
        .expect("cleanup host tombstone");
    query("DELETE FROM agent_pairing_requests WHERE request_id=?1")
        .bind(stale_request_id)
        .execute(&pool)
        .await
        .expect("cleanup stale pairing");
    for invite in [&first_invite, &stale_invite, &second_invite] {
        query("DELETE FROM agent_instance_invites WHERE invite_id=?1")
            .bind(invite["request_id"].as_str().unwrap())
            .execute(&pool)
            .await
            .expect("cleanup invite");
    }
}
