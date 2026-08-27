use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Instant,
};

use tokio::sync::Mutex;

pub const REPORT_BUCKET_CAPACITY: f64 = 64.0;
pub const REPORT_BUCKET_REFILL_PER_SECOND: f64 = 16.0;

/// Authentication admission and rate-limit state owned by host monitoring.
#[derive(Clone)]
pub struct MonitoringState {
    pub pairing_attempts: Arc<Mutex<VecDeque<Instant>>>,
    pub pairing_attempts_by_ip: Arc<Mutex<HashMap<std::net::IpAddr, VecDeque<Instant>>>>,
    pub report_auth_attempts: Arc<Mutex<VecDeque<Instant>>>,
    pub report_auth_attempts_by_ip: Arc<Mutex<HashMap<std::net::IpAddr, VecDeque<Instant>>>>,
    report_buckets: Arc<Mutex<HashMap<String, TokenBucket>>>,
}

impl MonitoringState {
    pub(crate) fn new() -> Self {
        Self {
            pairing_attempts: Arc::new(Mutex::new(VecDeque::new())),
            pairing_attempts_by_ip: Arc::new(Mutex::new(HashMap::new())),
            report_auth_attempts: Arc::new(Mutex::new(VecDeque::new())),
            report_auth_attempts_by_ip: Arc::new(Mutex::new(HashMap::new())),
            report_buckets: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Record one authenticated report without scanning all historical hosts.
    pub async fn allow_report(&self, host_id: &str, now: Instant) -> bool {
        self.report_buckets
            .lock()
            .await
            .entry(host_id.to_string())
            .or_insert_with(TokenBucket::new)
            .try_take(now)
    }

    pub async fn prune_report_buckets(&self, now: Instant) -> usize {
        let mut buckets = self.report_buckets.lock().await;
        let before = buckets.len();
        buckets.retain(|_, bucket| !bucket.is_idle(now));
        before - buckets.len()
    }

    pub async fn forget_host(&self, host_id: &str) {
        self.report_buckets.lock().await.remove(host_id);
    }
}

#[derive(Debug)]
struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    fn new() -> Self {
        Self {
            tokens: REPORT_BUCKET_CAPACITY,
            last_refill: Instant::now(),
        }
    }

    fn try_take(&mut self, now: Instant) -> bool {
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens =
            (self.tokens + elapsed * REPORT_BUCKET_REFILL_PER_SECOND).min(REPORT_BUCKET_CAPACITY);
        self.last_refill = now;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }

    fn is_idle(&self, now: Instant) -> bool {
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens + elapsed * REPORT_BUCKET_REFILL_PER_SECOND >= REPORT_BUCKET_CAPACITY
    }
}
