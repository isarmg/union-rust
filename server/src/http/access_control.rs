//! API 访问控制中间件。
//!
//! 这里集中处理会话认证、SSE 短效票据、数据库可用性检查和 Cookie CSRF 防护。
//! 路由模块只负责请求解析和业务调用，不重复实现安全策略。

use std::time::{Duration, Instant};

use axum::{
    extract::{Request, State},
    http::Method,
    middleware::Next,
    response::Response,
};

use crate::{
    error::AppError,
    infra::database,
    state::{AppState, LocalSession},
};

use crate::auth::http as auth;

const DATABASE_HEALTH_TTL: Duration = Duration::from_secs(1);

pub(super) async fn require_auth(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let path = request.uri().path().to_string();

    if is_public_path(&path) {
        return Ok(next.run(request).await);
    }

    // EventSource 不支持自定义请求头，因此 SSE 使用一次性短效票据。
    if path == "/api/events"
        && let Some(ticket) = sse_ticket(request.uri().query()).map(str::to_owned)
    {
        return authenticate_sse(&state, &ticket, request, next).await;
    }

    let token = auth::session_cookie(request.headers()).ok_or(AppError::Unauthorized)?;
    let session = auth::local_session(&state, &token).await?;
    let username = session.username.clone();

    if path == "/api/events" {
        let cancellation = auth::sse_session_cancellation(&state, &token).await?;
        request.extensions_mut().insert(cancellation);
    }

    ensure_database_available(&state, &path).await?;
    ensure_csrf_protected(&request, &session)?;

    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    Ok(database::with_audit_context(
        database::AuditContext {
            actor: username,
            request_id,
        },
        next.run(request),
    )
    .await)
}

fn is_public_path(path: &str) -> bool {
    matches!(path, "/api/health" | "/api/ready" | "/api/auth/login")
}

fn sse_ticket(query: Option<&str>) -> Option<&str> {
    query?
        .split('&')
        .find_map(|parameter| parameter.strip_prefix("ticket="))
}

/// 用一张短效票据换取 SSE 连接。
///
/// 票据是一次性的（`remove` 而非 `get`），且必须同时满足两个条件：
///
/// 1. 签发后未超过 60 秒；
/// 2. **签发它的会话此刻仍然有效**。
///
/// 第二条不可省。只校验时效的话，票据就成了一张与账号状态脱钩的通行证：管理员
/// 注销或改密（后者会踢掉该账号的全部会话）之后，此前签发的票据在剩余窗口内
/// 依然能建立连接——"我已经登出了"与"连接确实断了"之间因此存在一个静默的窗口。
async fn authenticate_sse(
    state: &AppState,
    ticket: &str,
    mut request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let issued = state.auth.sse_tickets.lock().await.remove(ticket);
    let Some(issued) = issued.filter(|entry| entry.issued_at.elapsed() < Duration::from_secs(60))
    else {
        return Err(AppError::Unauthorized);
    };
    // 会话已注销或已因改密失效时，这里返回 Unauthorized；成功时把该会话的
    // 撤销通知注入 handler，使已经建立的长连接也会随注销/改密立即结束。
    let cancellation = auth::sse_session_cancellation(state, &issued.session_token).await?;
    request.extensions_mut().insert(cancellation);

    ensure_database_available(state, "/api/events").await?;
    Ok(next.run(request).await)
}

async fn ensure_database_available(state: &AppState, path: &str) -> Result<(), AppError> {
    if requires_database(path) && !database_available(state).await {
        return Err(AppError::DatabaseUnavailable(
            "本地数据库暂不可用，请检查数据目录、数据库文件身份、schema、I/O 和文件权限"
                .to_string(),
        ));
    }
    Ok(())
}

pub(crate) async fn database_available(state: &AppState) -> bool {
    // A cached successful SQL probe must never hide that the canonical path
    // was unlinked or atomically replaced while SQLite kept the old inode
    // open. Check around the cache lookup and invalidate on either mismatch so
    // restoring the old inode cannot revive a stale success snapshot.
    if state.database_identity().verify().is_err() {
        clear_database_health_if_uncontended(state);
        return false;
    }
    let available = cached_database_available_with(state.database_health.as_ref(), || async {
        database::ping(state.db().as_ref(), state.database_identity())
            .await
            .is_ok()
    })
    .await;
    if state.database_identity().verify().is_err() {
        clear_database_health_if_uncontended(state);
        return false;
    }
    available
}

fn clear_database_health_if_uncontended(state: &AppState) {
    // Identity invalidation is sticky for the lifetime of the process, so a
    // cached result can never make a later request healthy again. Do not wait
    // behind an in-flight schema probe merely to clear an unreachable cache
    // entry: readiness must fail closed immediately after detecting the file
    // identity mismatch.
    if let Ok(mut health) = state.database_health.try_lock() {
        *health = None;
    }
}

async fn cached_database_available_with<Probe, ProbeFuture>(
    cache: &tokio::sync::Mutex<Option<crate::state::DatabaseHealthSnapshot>>,
    probe: Probe,
) -> bool
where
    Probe: FnOnce() -> ProbeFuture,
    ProbeFuture: std::future::Future<Output = bool>,
{
    // Keep the lock across the stale probe. Besides protecting the snapshot,
    // this is a single-flight gate: concurrent public readiness checks at the
    // TTL boundary must not all acquire a scarce database connection.
    let mut cached = cache.lock().await;
    if let Some(snapshot) = cached.as_ref()
        && snapshot.checked_at.elapsed() < DATABASE_HEALTH_TTL
    {
        return snapshot.available;
    }

    let available = probe().await;
    *cached = Some(crate::state::DatabaseHealthSnapshot {
        checked_at: Instant::now(),
        available,
    });
    available
}

/// 双提交（double-submit）CSRF 校验。
///
/// 前端从非 HttpOnly 的 CSRF cookie 中读取令牌，回填到 `x-csrf-token` 请求头；
/// 服务端把请求头与**该会话**存储的令牌做恒定时间比较。
///
/// 令牌是每会话随机的，而不是固定值（如 `x-csrf-token: 1`）。固定值依赖"浏览器禁止
/// 跨源发送自定义头"这一外部前提；随机令牌即便将来误配 CORS 允许了任意请求头，
/// 攻击者仍需要猜出令牌值，而跨站脚本读不到本站的 cookie。
fn ensure_csrf_protected(request: &Request, session: &LocalSession) -> Result<(), AppError> {
    if !is_state_changing(request.method()) {
        return Ok(());
    }
    let presented = request
        .headers()
        .get(auth::CSRF_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !session.csrf_token_matches(presented) {
        return Err(AppError::Forbidden(
            "missing or invalid CSRF protection header".to_string(),
        ));
    }
    Ok(())
}

fn is_state_changing(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn requires_database(path: &str) -> bool {
    path == "/api/audit-logs"
        || path == "/api/events"
        || path == "/api/events/ticket"
        || path.starts_with("/api/services")
        || path.starts_with("/api/monitoring")
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use futures_util::FutureExt;
    use sqlx_core::query::query;

    use super::*;

    async fn file_backed_state() -> (tempfile::TempDir, std::path::PathBuf, AppState) {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let path = directory.path().join("unionc.db");
        let mut settings = crate::config::Settings::default();
        settings.database.url = path.display().to_string();
        let pool = database::connect(&settings)
            .await
            .expect("connect database");
        database::initialize_schema(&pool)
            .await
            .expect("initialize schema");
        let state = AppState::new(
            settings,
            pool,
            "unused".to_string(),
            crate::config::LocalConfig {
                application_version: env!("CARGO_PKG_VERSION").to_string(),
                admin_username: "admin".to_string(),
                admin_password_hash: "unused".to_string(),
            },
            crate::system::ResourceMonitor::frozen(Default::default()),
        )
        .expect("capture file database identity");
        (directory, path, state)
    }

    #[tokio::test]
    async fn fresh_success_cache_cannot_hide_a_replaced_database_file() {
        let (_directory, path, state) = file_backed_state().await;
        assert!(database_available(&state).await, "prime healthy cache");
        let displaced = path.with_extension("displaced");
        std::fs::rename(&path, &displaced).expect("displace live database path");
        std::fs::File::create(&path).expect("install different file at canonical path");

        assert!(
            !database_available(&state).await,
            "a fresh SQL success snapshot must not hide an inode replacement"
        );
        assert!(
            state.database_health.lock().await.is_none(),
            "identity mismatch must invalidate the SQL health snapshot"
        );

        std::fs::remove_file(&path).expect("remove replacement file");
        std::fs::rename(&displaced, &path).expect("restore original inode");
        assert!(
            !database_available(&state).await,
            "an observed live-file replacement must require a full restart"
        );
        let pool = state.db();
        let acquisition = tokio::time::timeout(Duration::from_millis(250), pool.acquire()).await;
        assert!(
            !matches!(acquisition, Ok(Ok(_))),
            "the pool and AppState must share sticky invalidation"
        );
        state.db().close().await;
    }

    #[tokio::test]
    async fn database_identity_failure_does_not_wait_for_the_health_probe_lock() {
        let (_directory, path, state) = file_backed_state().await;
        assert!(database_available(&state).await, "prime healthy cache");
        let health_guard = state.database_health.lock().await;
        let displaced = path.with_extension("displaced");
        std::fs::rename(&path, &displaced).expect("displace live database path");
        std::fs::File::create(&path).expect("install different file at canonical path");

        let available = database_available(&state)
            .now_or_never()
            .expect("identity failure must complete on its first poll");
        assert!(!available);

        drop(health_guard);
        state.db().close().await;
    }

    #[tokio::test]
    async fn stale_probe_rejects_schema_changes_that_leave_metadata_untouched() {
        let (_directory, _path, state) = file_backed_state().await;
        assert!(database_available(&state).await, "prime healthy cache");
        query("DROP INDEX idx_audit_logs_created_at")
            .execute(state.db().as_ref())
            .await
            .expect("alter schema without changing metadata");
        *state.database_health.lock().await = None;

        assert!(
            !database_available(&state).await,
            "the runtime probe must verify the exact schema, not SELECT 1 or metadata alone"
        );
        state.db().close().await;
    }

    #[tokio::test]
    async fn stale_database_health_probe_is_single_flight() {
        let cache = Arc::new(tokio::sync::Mutex::new(None));
        let calls = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();

        let first = tokio::spawn({
            let cache = cache.clone();
            let calls = calls.clone();
            let release = release.clone();
            async move {
                cached_database_available_with(cache.as_ref(), || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let _ = started_tx.send(());
                    release.acquire().await.expect("release probe").forget();
                    true
                })
                .await
            }
        });

        started_rx.await.expect("first probe did not start");
        assert!(
            cache.try_lock().is_err(),
            "the cache lock must remain held while the stale probe is in flight"
        );

        let mut followers = Vec::new();
        for _ in 0..31 {
            let cache = cache.clone();
            let calls = calls.clone();
            followers.push(tokio::spawn(async move {
                cached_database_available_with(cache.as_ref(), || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    false
                })
                .await
            }));
        }
        tokio::task::yield_now().await;
        release.add_permits(1);

        assert!(first.await.expect("join first health probe"));
        for follower in followers {
            assert!(follower.await.expect("join follower health probe"));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fresh_database_health_is_reused_and_stale_health_is_refreshed() {
        let cache = tokio::sync::Mutex::new(Some(crate::state::DatabaseHealthSnapshot {
            checked_at: Instant::now(),
            available: false,
        }));
        let calls = AtomicUsize::new(0);

        let fresh = cached_database_available_with(&cache, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            true
        })
        .await;
        assert!(!fresh);
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        cache.lock().await.as_mut().unwrap().checked_at =
            Instant::now() - DATABASE_HEALTH_TTL - Duration::from_millis(1);
        let refreshed = cached_database_available_with(&cache, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            true
        })
        .await;
        assert!(refreshed);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(cache.lock().await.as_ref().unwrap().available);
    }
}
