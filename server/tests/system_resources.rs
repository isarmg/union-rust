//! `/api/system/resources` 采样正确性。
//!
//! 这里守护两条容易被破坏的性质：
//!
//! 1. **读取不能触发采样**：每次调用都新建 `System::new_all()` 再 `refresh_all()`，
//!    不但遍历全部进程，CPU 两次刷新之间还只隔几微秒，读数没有稳定差值窗口。
//!
//! 2. **并发观察者互相吃掉增量**：吞吐类指标是"读取即消费"的差值。把采样放在 HTTP
//!    handler 里，两个浏览器标签同时轮询时，后一个请求的差值窗口只剩几十毫秒，读数
//!    直接塌成 0：
//!
//!    ```text
//!    观察者 A:            rx=620  tx=634  disk_w=8570562
//!    观察者 B（100ms 后）: rx=0    tx=0    disk_w=0
//!    ```
//!
//! 现在采样集中在唯一的后台任务里，读路径只取快照。下面的用例分别锁住这两点。
//!
//! 测试刻意不通过自旋线程断言“负载必须上升若干百分点”：共享 CI 主机可能已满载，
//! 容器也可能受 CPU quota 限制，这种断言验证的是运行环境而不是代码，天然会抖动。

use std::time::{Duration, Instant};

use unionc::system::ResourceMonitor;

/// 测试用采样周期。压到远小于生产的 2 秒，使整个用例在几秒内跑完。
const TEST_INTERVAL: Duration = Duration::from_millis(250);
#[tokio::test(flavor = "multi_thread")]
async fn sampled_cpu_is_bounded_and_reads_stay_cheap() {
    let monitor = ResourceMonitor::start_with_interval(TEST_INTERVAL).await;

    // sysinfo 的差值采样结果必须始终是有限百分比。这里不对共享 CI 主机制造或假设负载。
    let cpu = monitor.snapshot().cpu_usage_percent;
    assert!(
        cpu.is_finite() && (0.0..=100.0).contains(&cpu),
        "CPU 读数必须是 0..=100 的有限值，实际为 {cpu}"
    );

    // 读路径必须是纯内存操作。
    // 读快照不再触碰 /proc，1000 次读取应当在毫秒量级完成。
    let started = Instant::now();
    for _ in 0..1000 {
        let _ = monitor.snapshot();
    }
    let per_call = started.elapsed() / 1000;
    assert!(
        per_call < Duration::from_micros(100),
        "单次快照读取耗时 {per_call:?}，读路径疑似仍在真正采样"
    );
}

/// 回归 P0-1：多个并发观察者必须读到**同一份**读数。
///
/// 请求即采样时，第二个观察者会把第一个的增量窗口吃掉，吞吐读数直接归零。
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_viewers_all_see_the_same_reading() {
    let monitor = ResourceMonitor::start_with_interval(TEST_INTERVAL).await;
    // 制造一点真实的磁盘/网络活动，让吞吐不至于恒为 0。
    for _ in 0..64 {
        let _ = std::fs::read_to_string("/proc/diskstats");
    }
    tokio::time::sleep(TEST_INTERVAL * 3).await;

    // 20 个"浏览器标签"在同一瞬间读取。
    let readers: Vec<_> = (0..20)
        .map(|_| {
            let monitor = monitor.clone();
            tokio::spawn(async move { monitor.snapshot() })
        })
        .collect();

    let mut baseline: Option<(u64, u64, f32)> = None;
    for reader in readers {
        let snapshot = reader.await.expect("reader task");
        let reading = (
            snapshot.network.total_bytes_per_second,
            snapshot.disk_throughput.total_bytes_per_second,
            snapshot.cpu_usage_percent,
        );
        match baseline {
            None => baseline = Some(reading),
            Some(expected) => assert_eq!(
                reading, expected,
                "并发观察者读到了不同的快照——采样又回到了读取即消费的模型"
            ),
        }
    }

    // 连续两次读取之间不得因为"被前一次消费掉"而归零。
    let first = monitor.snapshot();
    let second = monitor.snapshot();
    assert_eq!(
        first.network.total_bytes_per_second, second.network.total_bytes_per_second,
        "相邻两次读取的吞吐必须一致；归零说明读取消费掉了增量窗口"
    );
    assert_eq!(
        first.disk_throughput.total_bytes_per_second,
        second.disk_throughput.total_bytes_per_second
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_totals_are_consistent() {
    let monitor = ResourceMonitor::start_with_interval(TEST_INTERVAL).await;
    let resources = monitor.snapshot();
    assert!(resources.memory_total_kib > 0, "总内存应为正数");
    assert!(
        resources.memory_used_kib <= resources.memory_total_kib,
        "已用内存 {} KiB 不应超过总内存 {} KiB",
        resources.memory_used_kib,
        resources.memory_total_kib
    );
}
