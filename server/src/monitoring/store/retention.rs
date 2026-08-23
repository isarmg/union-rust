/// 单批删除的行数。
///
/// 取 10_000 是在两种成本之间取平衡：批次太小则往返次数与事务开销占比过高，
/// 太大则又回到"一个长事务"的老问题。
const RETENTION_BATCH: i64 = 10_000;

/// 按保留期清理历史报告。
///
/// **保留每台主机当前的最新一份报告**，即使它已超出保留期。报文体只存在于本表，
/// 且只有最新一份非空，详情接口通过 `latest_report_id` 关联到它；若把这一份也删掉，
/// 长期离线主机的详情页就会变成空白。这类主机每台只多留一行，代价可以忽略。
///
/// 外键的 `ON DELETE SET NULL` 是第二道保险：即便这里的条件将来被改错，也只会让
/// 引用变成 NULL，而不会留下悬空 id。
pub async fn prune_monitoring_history(pool: &DbPool, retention_days: i64) -> anyhow::Result<u64> {
    let retention_days = retention_days.clamp(1, 3650);
    let cutoff = database::to_epoch_micros(Utc::now() - chrono::Duration::days(retention_days));
    let mut removed = 0_u64;

    // 分批删除，而不是一条 DELETE 扫完整张表。
    //
    // 按 100 台 / 10 秒 / 保留 30 天计算，每天约清掉 86 万行。SQLite
    // 只允许一个写者：一条大 DELETE 会长时间占用写锁、制造巨大 WAL，并让
    // Agent 上报在 busy timeout 内排队。每批使用独立的短 `BEGIN IMMEDIATE`
    // 事务，批次间释放写锁，WAL 模式下读路径仍可继续。
    //
    // DELETE 后的页面进入 freelist 并供后续写入复用，不会立即缩小数据库
    // 文件；若运维上确实需要把大量空间归还给文件系统，应在低峰窗口单独执行
    // VACUUM，而不应把它放进日常保留期任务。
    // `cutoff` 在进入函数时固定，而正常报告的 received_at 由 Server 写入当前时间，
    // 因此待删集合是有限的。必须持续到不足一批；若在固定批数处返回并照常睡 24 小时，
    // 合法写入速率高于该上限时，超期积压反而会永久增长。
    loop {
        let mut tx = database::begin_write(pool).await?;
        let affected = query(
            r#"
            DELETE FROM agent_metric_reports
            WHERE report_id IN (
                SELECT r.report_id
                FROM agent_metric_reports AS r
                WHERE r.received_at < ?1
                  AND NOT EXISTS (
                      SELECT 1 FROM monitored_hosts AS h WHERE h.latest_report_id = r.report_id
                  )
                LIMIT ?2
            )
            "#,
        )
        .bind(cutoff)
        .bind(RETENTION_BATCH)
        .execute(tx.connection())
        .await?
        .rows_affected();
        tx.commit().await?;

        removed = removed
            .checked_add(affected)
            .ok_or_else(|| anyhow::anyhow!("monitoring retention removed-row count overflow"))?;
        // 不足一批说明已经删干净了。
        if affected < RETENTION_BATCH as u64 {
            break;
        }
        // 批次之间让出一小段时间，让排队的上报获得写锁。
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Ok(removed)
}
