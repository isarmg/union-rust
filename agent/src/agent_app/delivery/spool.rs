/// 单类 spool 磁盘操作的健康度跟踪。
///
/// 单次 I/O 失败（磁盘瞬时写满、目录被误删、权限被改动）不应终止一个常驻守护进程：
/// 退出只会表现为反复崩溃重启，且期间连内存直传都停了。这里改为降级续跑，只有在
/// **同类操作连续**失败到阈值时才退出，把持续性故障交给服务管理器处理。主循环为
/// 读、写和补传各持有一个实例，避免“读取成功”掩盖“持续不可写”。
#[derive(Default)]
struct SpoolHealth {
    consecutive_failures: u32,
}

impl SpoolHealth {
    /// 连续失败多少次后放弃。按 10 秒采集间隔算约合 15 分钟持续故障。
    const MAX_CONSECUTIVE_FAILURES: u32 = 100;

    fn record_success(&mut self) {
        self.consecutive_failures = 0;
    }

    /// 记录一次失败。仅当连续失败达到阈值时才返回 `Err`（从而终止主循环）。
    fn record_failure(
        &mut self,
        operation: &str,
        error: &dyn std::fmt::Display,
    ) -> anyhow::Result<()> {
        self.consecutive_failures += 1;
        warn!(
            consecutive_failures = self.consecutive_failures,
            "{operation}失败，已降级继续运行：{error}"
        );
        if self.consecutive_failures >= Self::MAX_CONSECUTIVE_FAILURES {
            anyhow::bail!(
                "spool 连续 {} 次操作失败，判定为持续性故障；退出并交由服务管理器处理",
                self.consecutive_failures
            );
        }
        Ok(())
    }

    /// 尝试把报文写入 spool。写不进去时丢弃该报文并继续，而不是终止进程。
    fn try_enqueue(&mut self, spool: &Spool, report: &AgentReport) -> anyhow::Result<()> {
        match spool.enqueue(report) {
            Ok(()) => {
                self.record_success();
                Ok(())
            }
            Err(error) => {
                self.record_failure("写入 spool", &error)?;
                warn!(report_id = %report.report_id, "本次采样未能持久化，已丢弃");
                Ok(())
            }
        }
    }
}

/// 补传 spool 中积压的报文。
///
/// 返回值区分四种结局：队列排空、32 条批次额度用尽、保留具体性质的网络失败，以及
/// spool 自身的磁盘 I/O 故障。批次边界会主动让出调度，但下一批无需等待采样 ticker。
enum FlushOutcome {
    Drained,
    BatchComplete,
    Failed(unionc_agent::transport::SendError),
}

async fn flush_spool(
    spool: &Spool,
    reporter: &Reporter,
    otlp_queue: Option<&OtlpQueue>,
) -> anyhow::Result<FlushOutcome> {
    // 每轮最多补传 32 个批次，避免长时间断线恢复后独占网络和采样线程。
    for _ in 0..32 {
        let Some(pending) = spool.oldest()? else {
            return Ok(FlushOutcome::Drained);
        };
        match reporter.send_unionc(&pending.report).await {
            Ok(()) => {
                // 顺序很重要：**先确认出队，再导出 OTLP**。
                //
                // 反过来的话，一旦 acknowledge 失败（文件已被删、权限变更等），
                // 报文会留在 spool 里，下一轮重新读取并再次导出，在 Collector
                // 侧产生重复数据点。先出队则最坏只是漏导一次——OTLP 本就是
                // 尽力而为的次要输出，漏一个点远好过重复计数。
                let report = pending.report.clone();
                spool.acknowledge(pending)?;
                if let Some(queue) = otlp_queue {
                    queue.try_export(&report);
                }
            }
            // 永久拒绝：确认出队并丢弃，否则队首这条会永远阻塞后面所有报文的补传。
            Err(error) if error.is_permanent() => {
                error!(
                    report_id = %pending.report.report_id,
                    "spool 中的报文被永久拒绝，已丢弃：{error}"
                );
                spool.acknowledge(pending)?;
            }
            Err(error) => return Ok(FlushOutcome::Failed(error)),
        }
    }
    Ok(FlushOutcome::BatchComplete)
}

pub(super) fn jitter(base: Duration, percent: u8) -> Duration {
    if percent == 0 {
        return base;
    }
    let range = percent as f64 / 100.0;
    let factor = (1.0 - range) + random::<f64>() * range * 2.0;
    Duration::from_secs_f64((base.as_secs_f64() * factor).max(0.05))
}
