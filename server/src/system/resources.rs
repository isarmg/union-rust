//! 系统资源采集。
//!
//! 读取 CPU、内存、磁盘和网络吞吐信息，不修改系统状态。数据来自 `sysinfo` crate
//! 与 `/proc/diskstats`。
//!
//! # 为什么是"单一采样器 + 快照"而不是"请求即采样"
//!
//! 吞吐类指标（网络、磁盘 IO）本质是**两次采样之间的差值**，而且是**读取即消费**的：
//! 一旦读走增量，计数基线就前移了。把采样放在 HTTP handler 里，每个请求都会消费掉
//! 全局采样器的增量——两个浏览器标签同时轮询时，后一个请求拿到的增量窗口只有几十
//! 毫秒，读数直接塌成 0：
//!
//! ```text
//! 观察者 A:            rx=620  tx=634  disk_w=8570562
//! 观察者 B（100ms 后）: rx=0    tx=0    disk_w=0        ← 同一时刻的真实吞吐
//! ```
//!
//! 若再把除数 `.max(1.0)` 钳到 1 秒，"窗口不足 1 秒"的读数还会被系统性低估，
//! 而不是被识别为无效（见 `MIN_ELAPSED_SECONDS`）。
//!
//! 这与 SSE 服务状态探测遇到的是同一个问题（见 `startup::start_service_status_probe`），
//! 因此采用同一套解法：**唯一的后台任务按固定周期采样，HTTP 只读快照**。读路径因此
//! 变成纯粹的内存读取，与并发观察者数量完全无关。

use std::{
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use sysinfo::{Disks, Networks, System};

use crate::system::{DiskInfo, DiskThroughput, NetworkThroughput, SystemResources};

/// 后台采样周期。前端以 10 秒轮询，2 秒的采样周期保证任何一次读取拿到的快照都足够新，
/// 同时给出稳定的差值窗口。
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

/// 磁盘挂载列表变动很慢，没必要每个采样周期都重新枚举。
const DISK_LIST_REFRESH: Duration = Duration::from_secs(60);

/// 差值窗口下限。仅用于**防止除零**，不承担平滑职责——把它取成 1.0 会让所有短于
/// 一秒的窗口被系统性低估（这正是重构前的缺陷）。与 Agent 侧
/// `collectors::per_second` 的取值保持一致。
const MIN_ELAPSED_SECONDS: f64 = 0.001;

/// 系统资源快照的共享句柄。
///
/// 克隆代价是一次 `Arc` 递增；读路径只做一次读锁 + 克隆快照结构体，不触碰 `sysinfo`。
#[derive(Clone)]
pub struct ResourceMonitor {
    latest: Arc<RwLock<SystemResources>>,
}

impl ResourceMonitor {
    /// 建立采样基线并返回句柄。
    ///
    /// CPU 使用率同样是差值指标：`sysinfo` 要求两次刷新至少间隔
    /// `MINIMUM_CPU_UPDATE_INTERVAL`，否则读数退化为开机以来的均值。这里先建立基线，
    /// 等满一个最小间隔后再采一次，使**首个请求**就能拿到有意义的读数。
    pub async fn start() -> Self {
        Self::start_with_interval(SAMPLE_INTERVAL).await
    }

    /// 同 `start()`，但可指定采样周期。
    ///
    /// 供测试把周期压到几百毫秒，快速验证后台快照与并发读取。周期不得短于
    /// `sysinfo` 要求的 CPU 最小刷新间隔，否则 CPU 读数会退化为开机均值。
    pub async fn start_with_interval(interval: Duration) -> Self {
        let interval = interval.max(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        let mut sampler = Sampler::new();
        tokio::time::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL).await;
        let first = sampler.sample();
        let monitor = Self {
            latest: Arc::new(RwLock::new(first)),
        };
        monitor.spawn_sampling_loop(sampler, interval);
        monitor
    }

    /// 构造一个不采样、只返回固定快照的句柄。
    ///
    /// 供集成测试装配 `AppState` 使用：测试关心的是路由与鉴权，不需要真的去读
    /// `/proc`，也不该在测试进程里留下一个后台采样循环。
    pub fn frozen(snapshot: SystemResources) -> Self {
        Self {
            latest: Arc::new(RwLock::new(snapshot)),
        }
    }

    /// 读取最近一次快照。**不触发任何采样**，因此并发观察者不会互相干扰。
    pub fn snapshot(&self) -> SystemResources {
        match self.latest.read() {
            Ok(latest) => latest.clone(),
            // 采样任务 panic 会毒化锁。宁可返回略旧的读数，也不要让所有监控请求失败。
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn spawn_sampling_loop(&self, mut sampler: Sampler, interval: Duration) {
        let latest = self.latest.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // 采样耗时偶尔超过周期时，跳过错过的 tick 而不是追赶补偿——补偿只会
            // 产生一串窗口极短的无意义差值。
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            ticker.tick().await; // 首个 tick 立即返回，基线已在 start() 中建立
            loop {
                ticker.tick().await;
                // sysinfo 的刷新是阻塞的系统调用（读 /proc、/sys），放到阻塞线程池，
                // 避免拖住运行时的工作线程。
                let sampled = tokio::task::spawn_blocking(move || {
                    let snapshot = sampler.sample();
                    (sampler, snapshot)
                })
                .await;
                let Ok((returned, snapshot)) = sampled else {
                    tracing::error!("系统资源采样任务异常退出，停止采样循环");
                    return;
                };
                sampler = returned;
                match latest.write() {
                    Ok(mut latest) => *latest = snapshot,
                    Err(poisoned) => *poisoned.into_inner() = snapshot,
                }
            }
        });
    }
}

/// 长生命周期采样器。
///
/// `sysinfo` 的对象必须复用：`System::new_all()` 每次都会枚举全部进程（遍历 /proc 下
/// 每个 PID），而本模块只需要 CPU 与内存两个标量；更关键的是，新建实例读到的 CPU
/// 使用率是开机以来的均值而非当前值。
struct Sampler {
    system: System,
    networks: Networks,
    disks: Disks,
    disk_io: DiskIoCounters,
    last_sample: Instant,
    last_disk_list_refresh: Instant,
}

impl Sampler {
    fn new() -> Self {
        let mut system = System::new();
        system.refresh_cpu_usage();
        system.refresh_memory();
        let now = Instant::now();
        Self {
            system,
            networks: Networks::new_with_refreshed_list(),
            disks: Disks::new_with_refreshed_list(),
            disk_io: DiskIoCounters::read(),
            last_sample: now,
            last_disk_list_refresh: now,
        }
    }

    fn sample(&mut self) -> SystemResources {
        let now = Instant::now();
        let elapsed = now
            .duration_since(self.last_sample)
            .as_secs_f64()
            .max(MIN_ELAPSED_SECONDS);
        self.last_sample = now;

        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.networks.refresh(true);

        if now.duration_since(self.last_disk_list_refresh) >= DISK_LIST_REFRESH {
            self.disks.refresh(true);
            self.last_disk_list_refresh = now;
        }

        SystemResources {
            cpu_usage_percent: self.system.global_cpu_usage(),
            memory_total_kib: self.system.total_memory() / 1024,
            memory_used_kib: self.system.used_memory() / 1024,
            network: network_throughput(&self.networks, elapsed),
            disk_throughput: self.disk_io.advance(elapsed),
            disks: self
                .disks
                .list()
                .iter()
                .map(|disk| DiskInfo {
                    name: disk.name().to_string_lossy().to_string(),
                    mount_point: disk.mount_point().to_string_lossy().to_string(),
                    total_bytes: disk.total_space(),
                    available_bytes: disk.available_space(),
                })
                .collect(),
        }
    }
}

/// `refresh` 之后，`data.received()` 是上一次刷新到本次刷新之间的字节数，
/// 因此除以同一段 `elapsed` 即得速率。
fn network_throughput(networks: &Networks, elapsed_seconds: f64) -> NetworkThroughput {
    let received: u64 = networks.values().map(|data| data.received()).sum();
    let transmitted: u64 = networks.values().map(|data| data.transmitted()).sum();
    let received_bytes_per_second = per_second(received, elapsed_seconds);
    let transmitted_bytes_per_second = per_second(transmitted, elapsed_seconds);
    NetworkThroughput {
        received_bytes_per_second,
        transmitted_bytes_per_second,
        total_bytes_per_second: received_bytes_per_second
            .saturating_add(transmitted_bytes_per_second),
    }
}

// ─── 磁盘 IO 吞吐量（读取 /proc/diskstats） ──────────────────────────────────

/// `/proc/diskstats` 的累计扇区数。与网络不同，这里的计数器是自开机累计的，
/// 需要自己做差值。
struct DiskIoCounters {
    read_sectors: u64,
    write_sectors: u64,
}

impl DiskIoCounters {
    const SECTOR_BYTES: u64 = 512;

    fn read() -> Self {
        let (read_sectors, write_sectors) = read_diskstats();
        Self {
            read_sectors,
            write_sectors,
        }
    }

    /// 读取当前计数器、与上次做差、并把基线推进到当前值。
    fn advance(&mut self, elapsed_seconds: f64) -> DiskThroughput {
        let (read_sectors, write_sectors) = read_diskstats();
        // 计数器可能因设备热插拔而回退，用 saturating_sub 兜底为 0 而不是回绕成天文数字。
        let read_delta = read_sectors.saturating_sub(self.read_sectors);
        let write_delta = write_sectors.saturating_sub(self.write_sectors);
        self.read_sectors = read_sectors;
        self.write_sectors = write_sectors;

        let read_bytes_per_second = per_second(
            read_delta.saturating_mul(Self::SECTOR_BYTES),
            elapsed_seconds,
        );
        let write_bytes_per_second = per_second(
            write_delta.saturating_mul(Self::SECTOR_BYTES),
            elapsed_seconds,
        );
        DiskThroughput {
            read_bytes_per_second,
            write_bytes_per_second,
            total_bytes_per_second: read_bytes_per_second.saturating_add(write_bytes_per_second),
        }
    }
}

/// 读取所有叶子物理磁盘的累计扇区数，返回 `(read_sectors, write_sectors)`。
/// 只统计整盘设备，排除分区、device-mapper 与软件 RAID 层——否则同一份 IO 会在
/// 逻辑设备和底层磁盘上各计一次。
fn read_diskstats() -> (u64, u64) {
    let Ok(content) = std::fs::read_to_string("/proc/diskstats") else {
        return (0, 0);
    };
    let mut read_total: u64 = 0;
    let mut write_total: u64 = 0;
    for line in content.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 14 {
            continue;
        }
        if is_whole_block_device(fields[2]) {
            read_total = read_total.saturating_add(fields[5].parse::<u64>().unwrap_or(0));
            write_total = write_total.saturating_add(fields[9].parse::<u64>().unwrap_or(0));
        }
    }
    (read_total, write_total)
}

fn is_whole_block_device(name: &str) -> bool {
    if name.starts_with("loop") || name.starts_with("zram") {
        return false;
    }
    if name.starts_with("nvme") || name.starts_with("mmcblk") {
        // 这两类整盘名称以数字结尾，分区才带 pN。
        return !name.rsplit_once('p').is_some_and(|(_, suffix)| {
            !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit())
        });
    }
    if name.starts_with("md") || name.starts_with("dm-") {
        return false;
    }
    name.chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_lowercase())
        && !name.chars().last().is_some_and(|ch| ch.is_ascii_digit())
}

fn per_second(delta: u64, elapsed_seconds: f64) -> u64 {
    (delta as f64 / elapsed_seconds.max(MIN_ELAPSED_SECONDS)).round() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_disks_are_counted_without_their_partitions() {
        for whole in ["sda", "nvme0n1", "mmcblk0", "vda"] {
            assert!(is_whole_block_device(whole), "{whole} 应被识别为整盘");
        }
        for partition in [
            "sda1",
            "nvme0n1p2",
            "mmcblk0p1",
            "loop0",
            "zram0",
            "md0",
            "dm-0",
        ] {
            assert!(
                !is_whole_block_device(partition),
                "{partition} 不应计入整盘统计"
            );
        }
    }

    /// 差值窗口下限只负责防除零，不得吞掉短窗口的真实量级。
    #[test]
    fn short_windows_are_not_systematically_underreported() {
        // 0.1 秒内传输 1000 字节 = 10000 B/s。把除数钳到 1.0 会算成 1000。
        assert_eq!(per_second(1_000, 0.1), 10_000);
        assert_eq!(per_second(1_000, 2.0), 500);
        // 除零仍然被挡住，且不会产生 inf/NaN。
        assert_eq!(per_second(1, 0.0), 1_000);
    }
}
