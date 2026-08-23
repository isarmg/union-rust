#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_jitter_is_exact() {
        assert_eq!(jitter(Duration::from_secs(10), 0), Duration::from_secs(10));
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_delivery_notification_channel_does_not_shift_sampling_cadence() {
        let (sender, _receiver) = mpsc::channel(1);
        let trigger = DeliveryTrigger { sender };
        let mut cadence = SamplingCadence::starting_now();
        let start = cadence.deadline();

        for index in 0..4 {
            tokio::time::sleep_until(cadence.deadline()).await;
            assert_eq!(
                tokio::time::Instant::now(),
                start + Duration::from_secs(index * 10),
                "a blocked delivery consumer must not move sampling tick {index}"
            );
            assert!(trigger.notify());
            cadence.schedule_next(Duration::from_secs(10), tokio::time::Instant::now());
        }
    }

    #[test]
    fn cadence_skips_an_overrun_instead_of_bursting_missed_samples() {
        let mut cadence = SamplingCadence::starting_now();
        let start = cadence.deadline();
        cadence.schedule_next(Duration::from_secs(10), start + Duration::from_secs(25));
        assert_eq!(cadence.deadline(), start + Duration::from_secs(35));
    }

    #[tokio::test(start_paused = true)]
    async fn a_ready_delivery_is_not_raced_against_its_expired_retry_timer() {
        let now = Instant::now();
        let (ready, next_retry) = delivery_timing(false, Some(now), now);
        assert!(ready);
        assert_eq!(next_retry, None);

        let deadline = next_retry.unwrap_or(now + Duration::from_secs(60));
        let delivered = tokio::select! {
            biased;
            completed = async {
                // A real HTTP request returns Pending while DNS/TCP/TLS makes progress.
                tokio::task::yield_now().await;
                true
            }, if ready => completed,
            _ = tokio::time::sleep_until(deadline.into()) => false,
        };

        assert!(
            delivered,
            "an expired retry timer cancelled a pending delivery future"
        );
    }

    #[test]
    fn a_future_delivery_retry_remains_a_timer_until_it_is_ready() {
        let now = Instant::now();
        let retry_at = now + Duration::from_secs(5);
        let (ready, next_retry) = delivery_timing(false, Some(retry_at), now);

        assert!(!ready);
        assert_eq!(next_retry, Some(retry_at));
    }

    #[test]
    fn authorization_blocks_both_delivery_and_its_retry_timer() {
        let now = Instant::now();
        let (ready, next_retry) = delivery_timing(true, Some(now), now);

        assert!(!ready);
        assert_eq!(next_retry, None);
    }

    #[test]
    fn persistent_pairing_snapshot_failures_back_off_and_cap() {
        let now = Instant::now();
        let (retry_at, next_backoff, delay) =
            pairing_failure_schedule(now, Duration::from_secs(1), 0);
        assert_eq!(delay, Duration::from_secs(1));
        assert_eq!(retry_at, now + Duration::from_secs(1));
        assert_eq!(next_backoff, Duration::from_secs(2));

        let (retry_at, next_backoff, delay) =
            pairing_failure_schedule(now, Duration::from_secs(300), 0);
        assert_eq!(delay, Duration::from_secs(300));
        assert_eq!(retry_at, now + Duration::from_secs(300));
        assert_eq!(next_backoff, Duration::from_secs(300));
    }

    #[tokio::test(start_paused = true)]
    async fn delivery_worker_shutdown_has_a_hard_upper_bound() {
        let worker = tokio::spawn(async {
            std::future::pending::<()>().await;
            Ok(())
        });
        let started = tokio::time::Instant::now();
        stop_delivery_worker(worker).await.unwrap();
        assert_eq!(
            tokio::time::Instant::now().duration_since(started),
            Duration::from_secs(5)
        );
    }

    #[tokio::test]
    async fn cancelled_one_shot_retains_the_current_report_for_idempotent_retry() {
        let directory = std::env::temp_dir().join(format!(
            "unionc-once-shutdown-{}",
            Uuid::new_v4()
        ));
        let spool = Spool::open(&directory, 1024 * 1024).unwrap();
        let mut sampler = SystemSampler::new();
        let report = sampler.collect(
            transient_host_identity(Uuid::new_v4()),
            10,
            0,
        );
        let (controller, shutdown) = unionc_agent::service::shutdown_channel();
        controller.request_shutdown();

        let operation = finish_before_shutdown(&shutdown, std::future::pending::<()>()).await;
        assert!(operation.is_none());
        assert_eq!(
            retain_once_report(&spool, &report).unwrap(),
            RunOnceOutcome::Shutdown
        );
        let pending = spool.oldest().unwrap().expect("cancelled report is durable");
        assert_eq!(pending.report.report_id, report.report_id);

        drop(spool);
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// 偶发 I/O 失败必须降级续跑，不能终止常驻进程。
    #[test]
    fn transient_spool_failures_do_not_stop_the_agent() {
        let mut health = SpoolHealth::default();
        for _ in 0..(SpoolHealth::MAX_CONSECUTIVE_FAILURES - 1) {
            health
                .record_failure("测试", &"disk full")
                .expect("未达阈值前必须继续运行");
        }
    }

    /// 但持续性故障要退出，交给服务管理器处理——否则会静默地一直丢数据。
    #[test]
    fn sustained_spool_failures_eventually_stop_the_agent() {
        let mut health = SpoolHealth::default();
        for _ in 0..(SpoolHealth::MAX_CONSECUTIVE_FAILURES - 1) {
            health.record_failure("测试", &"disk full").unwrap();
        }
        let error = health
            .record_failure("测试", &"disk full")
            .expect_err("达到阈值必须返回错误以终止主循环");
        assert!(
            error.to_string().contains("持续性故障"),
            "错误信息应说明这是持续性故障而非偶发，实际为：{error}"
        );
    }

    /// 中间只要成功一次，计数就归零——阈值针对的是**连续**失败。
    #[test]
    fn a_single_success_resets_the_failure_streak() {
        let mut health = SpoolHealth::default();
        for _ in 0..(SpoolHealth::MAX_CONSECUTIVE_FAILURES - 1) {
            health.record_failure("测试", &"transient").unwrap();
        }
        health.record_success();
        // 归零后应能再撑满一整轮，说明计数确实被重置了。
        for _ in 0..(SpoolHealth::MAX_CONSECUTIVE_FAILURES - 1) {
            health
                .record_failure("测试", &"transient")
                .expect("成功一次后计数应归零");
        }
    }
}
