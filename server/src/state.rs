//! Axum 路由共享状态。

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use tokio::sync::{Mutex, RwLock};

use crate::{
    config::{LocalConfig, Settings},
    infra::database::{DatabaseIdentity, DbPool},
};

#[derive(Clone)]
pub struct AppState {
    pub settings: Arc<Settings>,
    database: Arc<DbPool>,
    database_identity: Arc<DatabaseIdentity>,
    shutdown: tokio::sync::watch::Sender<bool>,
    pub database_health: Arc<Mutex<Option<DatabaseHealthSnapshot>>>,
    /// Monotonic process start marker. Wall-clock adjustments must never make
    /// a liveness uptime negative or jump it forwards.
    pub started_at: Instant,
    pub auth: AuthenticationState,
    pub services: ServiceStatusState,
    /// Product-neutral module catalog and external service adapter snapshots.
    pub platform: crate::platform::PlatformState,
}

/// 模块与平台适配器发布的服务状态快照。
///
/// 状态生产者在其自己的生命周期中完成采样并发布一次；所有 SSE 连接共享该快照和
/// 广播通道，浏览器连接数不会反向放大模块探测负载。
#[derive(Clone)]
pub struct ServiceStatusState {
    /// 每个模块独立贡献状态，避免不同模块或外部服务的探测结果互相覆盖。
    contributions: Arc<RwLock<BTreeMap<String, Vec<crate::system::ServiceStatus>>>>,
    /// 广播通道。容量很小即可：订阅者只关心最新状态，落后的直接跳到最新。
    pub events: tokio::sync::broadcast::Sender<Vec<crate::system::ServiceStatus>>,
}

impl ServiceStatusState {
    fn new() -> Self {
        let (events, _) = tokio::sync::broadcast::channel(4);
        Self {
            contributions: Arc::new(RwLock::new(BTreeMap::new())),
            events,
        }
    }

    pub async fn snapshot(&self) -> Vec<crate::system::ServiceStatus> {
        self.contributions
            .read()
            .await
            .values()
            .flatten()
            .cloned()
            .collect()
    }

    pub async fn publish(&self, source: &str, statuses: Vec<crate::system::ServiceStatus>) {
        let snapshot = {
            let mut contributions = self.contributions.write().await;
            contributions.insert(source.to_string(), statuses);
            contributions
                .values()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
        };
        let _ = self.events.send(snapshot);
    }
}

/// 一张 SSE 短效票据。
///
/// # 为什么要记住签发它的会话
///
/// 只存签发时间的话，票据就是一张与账号状态**完全脱钩**的通行证：管理员注销、
/// 改密（会踢掉该账号的全部会话）之后，此前签发的票据在剩余的有效期内依然能建立
/// SSE 连接。"我已经登出了"与"连接确实断了"之间因此存在一个静默的窗口。
///
/// 记下会话令牌后，`authenticate_sse` 可以在兑换票据时顺带确认那个会话仍然有效，
/// 使票据的生命周期不长于签发它的会话。
#[derive(Clone)]
pub struct SseTicket {
    /// 签发该票据的会话令牌。
    pub session_token: String,
    pub issued_at: Instant,
}

pub const SSE_TICKET_TTL: Duration = Duration::from_secs(60);
pub const MAX_PENDING_SSE_TICKETS: usize = 256;
pub const MAX_PENDING_SSE_TICKETS_PER_SESSION: usize = 8;

#[derive(Clone)]
pub struct AuthenticationState {
    pub sse_tickets: Arc<Mutex<HashMap<String, SseTicket>>>,
    /// Lazily-created cancellation channels for sessions with active SSE streams.
    pub session_revocations: Arc<Mutex<HashMap<String, tokio::sync::watch::Sender<bool>>>>,
    pub login_attempts: Arc<Mutex<LoginAttemptState>>,
    pub bcrypt_limit: Arc<tokio::sync::Semaphore>,
    /// Serializes the complete verify -> hash -> persist -> publish password change.
    pub password_change_gate: Arc<tokio::sync::Semaphore>,
    /// Linearizes password publication/session revocation with successful login insertion.
    /// Expensive bcrypt and disk I/O deliberately happen outside this short critical section.
    pub password_session_transition: Arc<Mutex<()>>,
    pub dummy_password_hash: Arc<String>,
    pub local_config: Arc<RwLock<LocalConfig>>,
    pub sessions: Arc<RwLock<HashMap<String, LocalSession>>>,
}

/// Cancellation handle injected into an authenticated SSE request.
#[derive(Clone)]
pub struct SseSessionCancellation {
    receiver: tokio::sync::watch::Receiver<bool>,
    expires_at: tokio::time::Instant,
}

impl SseSessionCancellation {
    pub fn new(
        receiver: tokio::sync::watch::Receiver<bool>,
        expires_at: tokio::time::Instant,
    ) -> Self {
        Self {
            receiver,
            expires_at,
        }
    }

    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow() || tokio::time::Instant::now() >= self.expires_at
    }

    pub async fn cancelled(&mut self) {
        if self.is_cancelled() {
            return;
        }
        loop {
            tokio::select! {
                () = tokio::time::sleep_until(self.expires_at) => return,
                changed = self.receiver.changed() => {
                    if changed.is_err() || *self.receiver.borrow_and_update() {
                        return;
                    }
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct LocalSession {
    pub username: String,
    pub expires_at: DateTime<Utc>,
    /// 与本会话绑定的 CSRF 令牌。
    ///
    /// 令牌**每会话随机**，而不是一个固定值（如 `x-csrf-token: 1`）。固定值的安全性
    /// 完全建立在"浏览器不允许跨源发送自定义头"这一外部前提上——一旦将来引入 CORS
    /// 中间件且配置为 `Allow-Headers: *`，防线会瞬间失效，且不会有任何测试失败。
    /// 随机令牌即使攻击者能跨源发送自定义头，也猜不出值。
    pub csrf_token: String,
}

impl LocalSession {
    /// 恒定时间比较请求头中的 CSRF 令牌，避免逐字节比较泄露前缀信息。
    pub fn csrf_token_matches(&self, candidate: &str) -> bool {
        let expected = self.csrf_token.as_bytes();
        let actual = candidate.as_bytes();
        if expected.len() != actual.len() {
            return false;
        }
        expected
            .iter()
            .zip(actual)
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
    }
}

#[derive(Debug, Default)]
pub struct LoginAttemptState {
    pub global: Vec<Instant>,
    /// 按 **(来源 IP, 用户名)** 复合键分桶，用于遏制针对单个账号的暴力破解。
    ///
    /// # 为什么键里必须带 IP
    ///
    /// 只按用户名计数（5 次/分钟）确实挡住了暴力破解，但同时制造了一个
    /// **任何人都能触发的账号锁定开关**：管理员用户名默认就是 `admin`，攻击者只要持续
    /// 发送失败的登录请求，就能让真正的管理员在整个窗口内无法登录。按 IP 的桶救不了，
    /// 因为两个桶是独立判定的——用户名桶先满，请求就已经被拒了。
    ///
    /// 这与本文件里 `by_ip` 注释描述的是**同一个反模式**：一个不区分来源的全局计数器，
    /// 既是防护也是武器。带上 IP 之后，洪水只会锁住攻击者自己的 (IP, 用户名) 组合，
    /// 而针对单账号的分布式爆破仍受 `by_ip` 与 `global` 两层约束。
    pub by_ip_username: HashMap<(std::net::IpAddr, String), Vec<Instant>>,
    /// 按来源 IP 分桶。全局桶单独存在时，攻击者只需用任意用户名打满全局配额，
    /// 就能让合法管理员在整个窗口内无法登录；分桶后洪水只影响攻击者自己的桶。
    pub by_ip: HashMap<std::net::IpAddr, Vec<Instant>>,
}

#[derive(Debug, Clone)]
pub struct DatabaseHealthSnapshot {
    pub checked_at: Instant,
    pub available: bool,
}

impl AppState {
    pub fn new(
        settings: Settings,
        db: DbPool,
        dummy_password_hash: String,
        local_config: LocalConfig,
    ) -> anyhow::Result<Self> {
        let database_identity = DatabaseIdentity::capture(&settings)?;
        let platform =
            crate::platform::PlatformState::from_environment(&local_config.admin_username)?;
        let (shutdown, _) = tokio::sync::watch::channel(false);
        Ok(Self {
            settings: Arc::new(settings),
            database: Arc::new(db),
            database_identity: Arc::new(database_identity),
            shutdown,
            database_health: Arc::new(Mutex::new(None)),
            started_at: Instant::now(),
            auth: AuthenticationState {
                sse_tickets: Arc::new(Mutex::new(HashMap::new())),
                session_revocations: Arc::new(Mutex::new(HashMap::new())),
                login_attempts: Arc::new(Mutex::new(LoginAttemptState::default())),
                bcrypt_limit: Arc::new(tokio::sync::Semaphore::new(4)),
                password_change_gate: Arc::new(tokio::sync::Semaphore::new(1)),
                password_session_transition: Arc::new(Mutex::new(())),
                dummy_password_hash: Arc::new(dummy_password_hash),
                local_config: Arc::new(RwLock::new(local_config)),
                sessions: Arc::new(RwLock::new(HashMap::new())),
            },
            services: ServiceStatusState::new(),
            platform,
        })
    }

    pub fn database_identity(&self) -> &DatabaseIdentity {
        self.database_identity.as_ref()
    }

    pub fn db(&self) -> Arc<DbPool> {
        self.database.clone()
    }

    /// Mark the process as shutting down before the HTTP server begins draining
    /// existing connections. `send_replace` keeps the value sticky for streams
    /// that subscribe concurrently with the shutdown signal.
    pub fn request_shutdown(&self) {
        self.shutdown.send_replace(true);
    }

    pub(crate) fn subscribe_shutdown(&self) -> tokio::sync::watch::Receiver<bool> {
        self.shutdown.subscribe()
    }
}
