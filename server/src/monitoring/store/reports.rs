pub async fn monitoring_host_for_token(
    pool: &DbPool,
    token_hash: &str,
) -> anyhow::Result<MonitoringTokenAuthentication> {
    let row = query(
        r#"
        SELECT h.host_id,c.revoked_at IS NOT NULL AS credential_revoked,h.lifecycle_status
        FROM agent_credentials c
        JOIN monitored_hosts h ON h.host_id=c.host_id
        WHERE c.token_hash=?1
        "#,
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(MonitoringTokenAuthentication::Unknown);
    };
    let credential_revoked: bool = row.try_get("credential_revoked")?;
    let lifecycle_status: String = row.try_get("lifecycle_status")?;
    // 403 is reserved for persistent host decommissioning. A credential
    // superseded by a current re-pair is no longer valid and behaves like an
    // unknown token (401), while a revoked host remains a terminal 403.
    if lifecycle_status != "active" {
        return Ok(MonitoringTokenAuthentication::Revoked);
    }
    if credential_revoked {
        return Ok(MonitoringTokenAuthentication::Unknown);
    }
    Ok(MonitoringTokenAuthentication::Active(
        row.try_get("host_id")?,
    ))
}

/// 写入一份报告。
///
/// # 报文体只为"最新一份"保留
///
/// `payload` 这一 JSON 文本列在整个代码库里**只有一个读取点**：详情接口经
/// `latest_report_id` 主键 JOIN 取当前最新的那一份（见 `host_select`）。列表接口和
/// 历史曲线都只读 9 个摘要数值列，从不触碰它。
///
/// 若为**每一份**历史报文都保留完整 JSON，其中只有每台主机的最新一份会被读到，
/// 其余会持续增加 SQLite 主文件、WAL 与备份的体积。按 100 台、10 秒周期、
/// 保留 30 天计算已是 2590 万行，因此这里只为每台主机保留最新一份报文体。
///
/// 因此这里在**写入前**就判断这份报文会不会成为最新：
/// * 会 → 带 payload 插入，并把上一份 latest 的 payload 置空；
/// * 不会（补传的历史报文）→ 直接以 NULL payload 插入。
///
/// 在写入前决策而不是"先写后清"，是为了避免白写 17 KiB、再立刻清空
/// 所带来的额外页面与 WAL 写入。
///
/// 代价是失去"回溯任意历史时刻的完整硬件快照"这一能力——但它当前没有任何接口暴露，
/// 若将来需要，正确的做法是按更长的间隔留样，而不是每个采集周期都留一份。
pub async fn store_monitoring_report(
    pool: &DbPool,
    report: &AgentReport,
) -> anyhow::Result<(bool, DateTime<Utc>)> {
    store_monitoring_report_inner(pool, report, None).await
}

/// Store an HTTP-authenticated report only if the exact credential remains
/// active at the write transaction's serialization point. The preliminary
/// lookup in the handler deliberately happens before JSON parsing, but a
/// concurrent re-pair may revoke that credential before this write begins.
pub async fn store_authenticated_monitoring_report(
    pool: &DbPool,
    report: &AgentReport,
    authenticated_token_hash: &str,
) -> anyhow::Result<(bool, DateTime<Utc>)> {
    store_monitoring_report_inner(pool, report, Some(authenticated_token_hash)).await
}

async fn store_monitoring_report_inner(
    pool: &DbPool,
    report: &AgentReport,
    authenticated_token_hash: Option<&str>,
) -> anyhow::Result<(bool, DateTime<Utc>)> {
    let host_id = canonical_uuid(&report.host.id)?;
    let report_id = canonical_uuid(&report.report_id)?;
    let capabilities = serde_json::to_string(&report.capabilities)?;
    // Capture receipt time only after this request owns the SQLite write transaction. A task
    // can wait behind a later-arriving writer; taking the timestamp before BEGIN IMMEDIATE would
    // let the older timestamp commit last and move host liveness backwards.
    let mut tx = database::begin_write(pool).await?;
    let received_at = Utc::now();
    let received_at_micros = database::to_epoch_micros(received_at);

    // 一条语句同时完成三件事：确认主机存在、取出当前的 latest 指针与时间戳，
    // 取出当前的 latest 指针与时间戳，以及**判断身份列是否真的变了**。
    //
    // 最后一项是为了避免写放大：identity 与 capabilities 几乎从不变化，若每份上报都把
    // 它们原样重写一遍，代价有二——
    //   1. SQLite 同一时刻只有一个写者，多一次 UPDATE 会直接延长事务持锁时间；
    //   2. 无条件重写未变的 `capabilities` JSON 会制造多余的 WAL 与页面写入。
    // 把比较放进这条已经存在的语句里，不增加任何往返。
    let current = query(
        r#"
        SELECT latest_report_id,
               latest_collected_at,
               (name           IS NOT ?2
             OR os             IS NOT ?3
             OR os_version     IS NOT ?4
             OR kernel_version IS NOT ?5
             OR arch           IS NOT ?6
             OR agent_version  IS NOT ?7
             OR capabilities   IS NOT ?8) AS identity_changed,
               EXISTS(
                   SELECT 1
                   FROM agent_credentials c
                   WHERE c.host_id=monitored_hosts.host_id
                     AND c.token_hash=?9
                     AND c.revoked_at IS NULL
               ) AS credential_active
        FROM monitored_hosts
        WHERE host_id=?1 AND lifecycle_status='active'
        "#,
    )
    .bind(&host_id)
    .bind(report.host.name.trim())
    .bind(report.host.os.trim())
    .bind(report.host.os_version.as_deref())
    .bind(report.host.kernel_version.as_deref())
    .bind(report.host.arch.trim())
    .bind(report.host.agent_version.trim())
    .bind(&capabilities)
    .bind(authenticated_token_hash)
    .fetch_optional(tx.connection())
    .await?
    .ok_or_else(|| anyhow::Error::new(StoreReportError::HostNotActive))?;
    let previous_latest: Option<String> = current.try_get("latest_report_id")?;
    let previous_collected: Option<i64> = current.try_get("latest_collected_at")?;
    let identity_changed: bool = current.try_get("identity_changed")?;
    let credential_active: bool = current.try_get("credential_active")?;
    if authenticated_token_hash.is_some() && !credential_active {
        return Err(anyhow::Error::new(StoreReportError::CredentialNotActive));
    }

    // 这份报文是否代表主机的"当前状态"。
    //
    // 断线恢复时 spool 会按时间升序补传一批**历史**报文，重放也可能把同一份再送一次。
    // 旧报文不该回写主机状态：否则一份小时前的报文能把刚更新的能力清单覆盖回去，
    // 而重写 `last_seen_at` 会让任何一次重放都把离线主机刷成 online。
    // SQLite persists timestamps at microsecond precision, so compare the same
    // normalized value used by history queries. For equal timestamps, history
    // orders `report_id DESC`; using the same secondary key here makes the
    // detail pointer independent of arrival order and guarantees it matches the
    // final history point.
    let collected_at_micros = database::to_epoch_micros(report.collected_at);
    let becomes_latest = previous_collected.is_none_or(|previous| {
        collected_at_micros > previous
            || (collected_at_micros == previous
                && previous_latest
                    .as_deref()
                    .is_none_or(|latest_id| report_id.as_str() > latest_id))
    });

    // 摘要列由 Rust 侧统一计算，SQL 只负责存放——聚合逻辑只有这一份实现。
    let metrics = report.metric_summary();
    // 只有会成为 latest 的报文才需要保留报文体。
    let payload = becomes_latest
        .then(|| serde_json::to_string(report))
        .transpose()?;
    let inserted = query(
        r#"
        INSERT INTO agent_metric_reports(
            report_id,host_id,schema_version,collected_at,interval_seconds,payload,
            cpu_usage_percent,memory_usage_percent,
            network_received_bytes_per_second,network_transmitted_bytes_per_second,
            disk_read_bytes_per_second,disk_written_bytes_per_second,
            max_temperature_celsius,gpu_utilization_percent,gpu_memory_usage_percent,
            received_at
        ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
        ON CONFLICT(report_id) DO NOTHING
        RETURNING received_at
        "#,
    )
    .bind(&report_id)
    .bind(&host_id)
    .bind(i32::from(report.schema_version))
    .bind(collected_at_micros)
    .bind(report.interval_seconds)
    .bind(payload.as_deref())
    .bind(metrics.cpu_usage_percent)
    .bind(metrics.memory_usage_percent)
    .bind(metrics.network_received_bytes_per_second)
    .bind(metrics.network_transmitted_bytes_per_second)
    .bind(metrics.disk_read_bytes_per_second)
    .bind(metrics.disk_written_bytes_per_second)
    .bind(metrics.max_temperature_celsius)
    .bind(metrics.gpu_utilization_percent)
    .bind(metrics.gpu_memory_usage_percent)
    .bind(received_at_micros)
    .fetch_optional(tx.connection())
    .await?;

    let (accepted, received_at) = if let Some(row) = inserted {
        (true, timestamp(&row, "received_at")?)
    } else {
        let row =
            query("SELECT received_at FROM agent_metric_reports WHERE report_id=?1 AND host_id=?2")
                .bind(&report_id)
                .bind(&host_id)
                .fetch_optional(tx.connection())
                .await?
                .ok_or_else(|| {
                    anyhow::Error::new(StoreReportError::ReportIdBelongsToAnotherHost)
                })?;
        (false, timestamp(&row, "received_at")?)
    };

    // A same-host duplicate is an idempotent replay, not a fresh heartbeat.
    // In particular it must not refresh last_seen_at or rewrite identity from
    // a body that happens to reuse an old report_id.
    if !accepted {
        tx.commit().await?;
        return Ok((false, received_at));
    }

    // 主机行的**唯一**一次写入。
    //
    // 拆成两条 UPDATE（INSERT 前写 identity + last_seen_at、INSERT 后写 latest_* 指针）
    // 更直观，但代价不只是多一次往返——两条 UPDATE 会延长 SQLite 单写者
    // 事务的持锁时间，并制造额外 WAL 与页面写入。
    //
    // 三组列各有各的写入条件，用 CASE 表达，避免为组合情况拼接 SQL：
    //   * identity/capabilities —— 仅在**确实变化**时写（见上面 SELECT 里的比较）。
    //     `ELSE <列自身>` 避免给未变的 JSON 文本重新赋值。
    //   * last_seen_at —— 只要这份**新插入**报文代表当前状态就刷新；同 report_id
    //     的重放已在上方提前返回，不能伪造一份新心跳。
    //   * latest_* 指针 —— 仅在报文确实新插入时推进。
    if becomes_latest {
        query(
            r#"
            UPDATE monitored_hosts SET
                name                    = CASE WHEN ?9 THEN ?2 ELSE name END,
                os                      = CASE WHEN ?9 THEN ?3 ELSE os END,
                os_version              = CASE WHEN ?9 THEN ?4 ELSE os_version END,
                kernel_version          = CASE WHEN ?9 THEN ?5 ELSE kernel_version END,
                arch                    = CASE WHEN ?9 THEN ?6 ELSE arch END,
                agent_version           = CASE WHEN ?9 THEN ?7 ELSE agent_version END,
                capabilities            = CASE WHEN ?9 THEN ?8 ELSE capabilities END,
                last_seen_at            = MAX(last_seen_at, ?14),
                latest_report_id        = CASE WHEN ?10 THEN ?11 ELSE latest_report_id END,
                latest_collected_at     = CASE WHEN ?10 THEN ?12 ELSE latest_collected_at END,
                latest_interval_seconds = CASE WHEN ?10 THEN ?13 ELSE latest_interval_seconds END
            WHERE host_id=?1
            "#,
        )
        .bind(&host_id)
        .bind(report.host.name.trim())
        .bind(report.host.os.trim())
        .bind(report.host.os_version.as_deref())
        .bind(report.host.kernel_version.as_deref())
        .bind(report.host.arch.trim())
        .bind(report.host.agent_version.trim())
        .bind(&capabilities)
        .bind(identity_changed)
        .bind(accepted)
        .bind(&report_id)
        .bind(collected_at_micros)
        .bind(report.interval_seconds)
        .bind(received_at_micros)
        .execute(tx.connection())
        .await?;

        // 上一份报文的 payload 从此不再有任何读取路径，立即释放。
        // 走主键定位，代价与表规模无关。
        if accepted
            && let Some(previous) = previous_latest.filter(|previous| previous != &report_id)
        {
            query("UPDATE agent_metric_reports SET payload=NULL WHERE report_id=?1")
                .bind(previous)
                .execute(tx.connection())
                .await?;
        }
    }
    tx.commit().await?;
    Ok((accepted, received_at))
}
