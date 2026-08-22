/// 采样一次，并保证之前由 `once`/`run` 留下的积压得到补传。
pub(super) async fn run_once(
    config: &AgentConfig,
    host: unionc_agent::HostIdentity,
    sampler: &mut SystemSampler,
    spool: &Spool,
    reporter: Reporter,
) -> anyhow::Result<()> {
    let pending = spool.pending_count()?;
    let report = sampler.collect(host.clone(), config.slow_interval_seconds, pending);
    // `flush_spool` 单轮最多发 32 份；once 是显式的一次性投递命令，因此循环到队列
    // 清空。若网络仍不可用，当前采样也入队后退出，下一次 once 可以继续恢复。
    while spool.pending_count()? > 0 {
        match flush_spool(spool, &reporter, None).await {
            Ok(FlushOutcome::Drained | FlushOutcome::BatchComplete) => {}
            Ok(FlushOutcome::Failed(error)) => {
                spool.enqueue(&report)?;
                return Err(anyhow::anyhow!(error)
                    .context("current report was retained while stored reports remain pending"));
            }
            Err(error) => {
                spool.enqueue(&report)?;
                return Err(error.context(
                    "current report was retained because the local spool could not be flushed",
                ));
            }
        }
    }

    let send = reporter.send_unionc(&report).await;
    if let Err(error) = send {
        if error.is_permanent() {
            return Err(anyhow::anyhow!(error)
                .context("report was rejected permanently and was not spooled"));
        }
        spool.enqueue(&report)?;
        return Err(anyhow::anyhow!(error).context("report was retained in the local spool"));
    }
    if let Err(error) = reporter.send_otlp(&report).await {
        warn!("optional OTLP export failed: {error}");
    }
    Ok(())
}

struct OtlpQueue {
    sender: mpsc::Sender<AgentReport>,
    worker: tokio::task::JoinHandle<()>,
}

impl OtlpQueue {
    fn spawn(reporter: Reporter) -> Self {
        // OTLP is an optional secondary output. A bounded worker prevents a slow
        // collector from delaying host sampling or primary UnionC delivery.
        let (sender, mut receiver) = mpsc::channel::<AgentReport>(128);
        let worker = tokio::spawn(async move {
            while let Some(report) = receiver.recv().await {
                if let Err(error) = reporter.send_otlp(&report).await {
                    warn!(report_id = %report.report_id, "optional OTLP export failed: {error}");
                }
            }
        });
        Self { sender, worker }
    }

    fn try_export(&self, report: &AgentReport) {
        if let Err(error) = self.sender.try_send(report.clone()) {
            warn!(report_id = %report.report_id, "optional OTLP queue rejected a report: {error}");
        }
    }

    fn abort(self) {
        // OTLP is best-effort and every primary report has already been
        // acknowledged before it reaches this queue. Do not let a Collector
        // timeout extend service shutdown by as much as 300 seconds.
        self.worker.abort();
    }
}
