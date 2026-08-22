//! Axum 路由共享状态。

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Instant,
};

use chrono::{DateTime, Utc};
use tokio::sync::{Mutex, RwLock};

use crate::{
    config::{LocalConfig, Settings, SunshineHostConfig},
    infra::database::DbPool,
};

#[derive(Clone)]
pub struct AppState {
    pub settings: Arc<Settings>,
    database: Arc<DbPool>,
    pub database_health: Arc<Mutex<Option<DatabaseHealthSnapshot>>>,
    pub started_at: DateTime<Utc>,
    pub hosts: HostState,
    pub auth: AuthenticationState,
    pub agents: AgentAuthenticationState,
    pub services: ServiceStatusState,
    /// 系统资源快照。与 `services` 同一个模式：唯一的后台任务采样，读路径只读快照。
    pub resources: crate::system::ResourceMonitor,
}

#[derive(Clone)]
pub struct AgentAuthenticationState {
    /// Browser-pairing creation/inspection/activation requests use a separate
    /// anonymous quota from high-frequency report polling.
    pub pairing_attempts: Arc<Mutex<VecDeque<Instant>>>,
    pub pairing_attempts_by_ip: Arc<Mutex<HashMap<std::net::IpAddr, VecDeque<Instant>>>>,
    /// 认证前的上报请求全局配额，避免随机 token 无限触发数据库查询。
    pub report_auth_attempts: Arc<Mutex<VecDeque<Instant>>>,
    /// 认证前的上报请求按来源 IP 配额。
    pub report_auth_attempts_by_ip: Arc<Mutex<HashMap<std::net::IpAddr, VecDeque<Instant>>>>,
    /// 按主机的上报令牌桶，防止单个（或凭据泄露的）主机打满数据库写入。
    pub report_buckets: Arc<Mutex<HashMap<String, TokenBucket>>>,
}

/// 令牌桶限流。
///
/// 之所以用令牌桶而非固定窗口计数：Agent 在断线恢复时会一次补传最多 32 个批次
/// （见 `agent/src/agent_app/delivery/spool.rs` 的 `flush_spool`），固定窗口会把这种
/// **合法**的突发
/// 误判为滥用。令牌桶允许攒下额度应对突发，同时约束长期平均速率。
#[derive(Debug)]
pub struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    /// 桶容量，即允许的最大突发量。取补传批量（32）的两倍留出余量。
    pub const CAPACITY: f64 = 64.0;
    /// 每秒补充的令牌数。必须**高于**合法 Agent 的最快上报速率：
    /// `AgentReport::validate()` 允许的最小间隔是 0.1 秒，即 10 次/秒。
    /// 取 16 保证正常配置永远不会被限流，同时把单主机写入封顶在 16 次/秒。
    pub const REFILL_PER_SECOND: f64 = 16.0;

    fn new() -> Self {
        Self {
            tokens: Self::CAPACITY,
            last_refill: Instant::now(),
        }
    }

    /// 取走一个令牌。返回 `false` 表示超出配额。
    fn try_take(&mut self, now: Instant) -> bool {
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * Self::REFILL_PER_SECOND).min(Self::CAPACITY);
        self.last_refill = now;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }

    /// 桶是否已回满。回满意味着该主机近期没有上报，条目可以回收。
    fn is_idle(&self, now: Instant) -> bool {
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens + elapsed * Self::REFILL_PER_SECOND >= Self::CAPACITY
    }
}

impl AgentAuthenticationState {
    /// 记录一次上报并判断是否超出该主机的配额。
    ///
    /// # 为什么这里不做回收
    ///
    /// 在**每一次上报**里都 `retain()` 一遍全部主机桶，等于在单一全局锁内做
    /// O(主机数) 的扫描。上报是全系统频率最高的写路径，那会给它挂上一个随部署
    /// 规模线性增长的串行段——恰恰在规模变大时最先劣化。
    ///
    /// 回收本身并不紧急（一个空闲桶只占几十字节），因此移交给
    /// `startup::start_memory_gc` 周期性执行，热路径只保留一次哈希查找。
    pub async fn allow_report(&self, host_id: &str, now: Instant) -> bool {
        self.report_buckets
            .lock()
            .await
            .entry(host_id.to_string())
            .or_insert_with(TokenBucket::new)
            .try_take(now)
    }

    /// 回收长期无上报的主机条目，避免 HashMap 随历史主机数无限增长。
    /// 由后台维护任务周期性调用，返回被回收的条目数。
    pub async fn prune_report_buckets(&self, now: Instant) -> usize {
        let mut buckets = self.report_buckets.lock().await;
        let before = buckets.len();
        buckets.retain(|_, bucket| !bucket.is_idle(now));
        before - buckets.len()
    }

    /// 主机注销后立即丢弃其限流桶。
    pub async fn forget_host(&self, host_id: &str) {
        self.report_buckets.lock().await.remove(host_id);
    }
}

#[derive(Clone)]
pub struct HostState {
    pub sunshine: Arc<RwLock<Vec<SunshineHostConfig>>>,
    /// 最近一次由后台任务完成的 Sunshine 连通性/认证探测。
    ///
    /// HTTP 列表与配置写入路径只读取这份快照，绝不直接访问 Sunshine。否则一个
    /// 接受 TCP 连接但不响应 HTTP 的主机会让新增、删除后的页面刷新卡满上游的
    /// 15 秒超时。主机配置变更时对应条目会先被替换为 `pending`，所以旧配置的
    /// “绿色”结果不会被沿用。
    pub sunshine_health: Arc<RwLock<HashMap<String, SunshineHostHealth>>>,
    /// 配置变更时唤醒唯一的后台探测任务；`Notify` 会保留一个 permit，因此即使
    /// 变更发生在一轮慢探测期间，完成后也会立即再跑一轮，不必等待定时周期。
    pub sunshine_health_refresh: Arc<tokio::sync::Notify>,
    pub settings_lock: Arc<Mutex<()>>,
}

/// 一台 Sunshine 主机的内存健康快照。
///
/// `None` 严格表示尚未完成当前配置的探测，而不是“不可达”。探测完成后两个布尔值
/// 都是 `Some`；当 TCP 不可达时 `connected` 固定为 `Some(false)`。
#[derive(Clone)]
pub struct SunshineHostHealth {
    pub reachable: Option<bool>,
    pub connected: Option<bool>,
    pub connection_error: Option<String>,
}

impl SunshineHostHealth {
    pub fn pending() -> Self {
        Self {
            reachable: None,
            connected: None,
            connection_error: Some("连接状态正在后台检测".to_string()),
        }
    }

    pub fn completed(reachable: bool, connection: &Result<(), String>) -> SunshineHostHealth {
        Self {
            reachable: Some(reachable),
            connected: Some(reachable && connection.is_ok()),
            connection_error: connection.as_ref().err().cloned(),
        }
    }
}

/// 服务状态的**唯一**探测结果，由后台任务单独维护。
///
/// 让每个 SSE 连接各自跑一遍探测循环，会使对每台 Sunshine 主机的 TCP 探测频率
/// 随浏览器标签数线性放大；且串行探测下 10 台离线主机就要 5 秒，直接把 5 秒的
/// 推送周期拖成 10 秒。"一次探测、多方订阅"把探测成本从 O(客户端数 × 主机数)
/// 降为 O(主机数)。
#[derive(Clone)]
pub struct ServiceStatusState {
    /// 最近一次探测结果。新建立的 SSE 连接先读它，避免等待下一个探测周期。
    pub snapshot: Arc<RwLock<Vec<crate::system::ServiceStatus>>>,
    /// 广播通道。容量很小即可：订阅者只关心最新状态，落后的直接跳到最新。
    pub events: tokio::sync::broadcast::Sender<Vec<crate::system::ServiceStatus>>,
}

impl ServiceStatusState {
    fn new() -> Self {
        let (events, _) = tokio::sync::broadcast::channel(4);
        Self {
            snapshot: Arc::new(RwLock::new(Vec::new())),
            events,
        }
    }
}

/// 一张 SSE 短效票据。
///
/// # 为什么要记住签发它的会话
///
/// 只存签发时间的话，票据就是一张与账号状态**完全脱钩**的通行证：管理员注销、
/// 改密（会踢掉其他设备的会话）之后，此前签发的票据在剩余的有效期内依然能建立
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

#[derive(Clone)]
pub struct AuthenticationState {
    pub sse_tickets: Arc<Mutex<HashMap<String, SseTicket>>>,
    /// Lazily-created cancellation channels for sessions with active SSE streams.
    pub session_revocations: Arc<Mutex<HashMap<String, tokio::sync::watch::Sender<bool>>>>,
    pub login_attempts: Arc<Mutex<LoginAttemptState>>,
    pub bcrypt_limit: Arc<tokio::sync::Semaphore>,
    /// Serializes the complete verify -> hash -> persist -> publish password change.
    pub password_change_gate: Arc<tokio::sync::Semaphore>,
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
        resources: crate::system::ResourceMonitor,
    ) -> Self {
        let sunshine_hosts = settings.sunshine.hosts.clone();
        Self {
            settings: Arc::new(settings),
            database: Arc::new(db),
            database_health: Arc::new(Mutex::new(None)),
            started_at: Utc::now(),
            hosts: HostState {
                sunshine: Arc::new(RwLock::new(sunshine_hosts)),
                sunshine_health: Arc::new(RwLock::new(HashMap::new())),
                sunshine_health_refresh: Arc::new(tokio::sync::Notify::new()),
                settings_lock: Arc::new(Mutex::new(())),
            },
            auth: AuthenticationState {
                sse_tickets: Arc::new(Mutex::new(HashMap::new())),
                session_revocations: Arc::new(Mutex::new(HashMap::new())),
                login_attempts: Arc::new(Mutex::new(LoginAttemptState::default())),
                bcrypt_limit: Arc::new(tokio::sync::Semaphore::new(4)),
                password_change_gate: Arc::new(tokio::sync::Semaphore::new(1)),
                dummy_password_hash: Arc::new(dummy_password_hash),
                local_config: Arc::new(RwLock::new(local_config)),
                sessions: Arc::new(RwLock::new(HashMap::new())),
            },
            agents: AgentAuthenticationState {
                pairing_attempts: Arc::new(Mutex::new(VecDeque::new())),
                pairing_attempts_by_ip: Arc::new(Mutex::new(HashMap::new())),
                report_auth_attempts: Arc::new(Mutex::new(VecDeque::new())),
                report_auth_attempts_by_ip: Arc::new(Mutex::new(HashMap::new())),
                report_buckets: Arc::new(Mutex::new(HashMap::new())),
            },
            services: ServiceStatusState::new(),
            resources,
        }
    }

    pub fn db(&self) -> Arc<DbPool> {
        self.database.clone()
    }
}
