/// 主机列表。**不读取任何完整报告 JSON**——指标来自 JOIN 到的摘要数值列。
///
/// # 为什么必须有上限
///
/// 不带 `LIMIT` 一次返回全部主机在几百台时无感，但它是一条**随部署规模线性增长**
/// 的响应：每台主机都带着 capabilities 数组，几千台时单次响应就到数 MB，
/// 而控制台每 10 秒轮询一次，每次都要付这个代价。
///
/// `COUNT(*) OVER()` 在同一次扫描里带出总数，因此仍是一次往返——调用方据此可以
/// 告诉用户"还有多少没显示"，而不是**静默截断**。截断而不告知比不分页更糟。
pub async fn list_monitored_hosts(
    pool: &DbPool,
    limit: i64,
    offset: i64,
) -> anyhow::Result<(Vec<StoredHost>, i64)> {
    // `host_id` 作为最终决胜键：`last_seen_at` 会随上报持续变化、`name` 可能重复，
    // 只有再补一个唯一列才能保证同一查询的分页顺序是确定的（相邻两页不重不漏）。
    let rows = query(&host_select(
        false,
        "ORDER BY h.last_seen_at DESC, h.name, h.host_id LIMIT ?1 OFFSET ?2",
    ))
    .bind(limit)
    .bind(offset.max(0))
    .fetch_all(pool)
    .await?;
    // 0 行时无从取窗口函数值，此时总数只能另算——但这只发生在空库或翻过头的页上，
    // 不是热路径。
    let total = match rows.first() {
        Some(row) => row.try_get::<i64, _>("total")?,
        None => query("SELECT COUNT(*) AS total FROM monitored_hosts")
            .fetch_one(pool)
            .await?
            .try_get("total")?,
    };
    let hosts = rows
        .into_iter()
        .map(|row| stored_host_from_row(row, false))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok((hosts, total))
}

/// 主机详情。这是唯一需要完整报告体的读路径，报文从 JOIN 到的
/// `agent_metric_reports.payload` 取得——报文体只此一份，且只为最新报告保留。
pub async fn get_monitored_host(
    pool: &DbPool,
    host_id: &str,
) -> anyhow::Result<Option<StoredHost>> {
    let host_id = canonical_uuid(host_id)?;
    let row = query(&host_select(true, "WHERE h.host_id=?1"))
        .bind(host_id)
        .fetch_optional(pool)
        .await?;
    row.map(|row| stored_host_from_row(row, true)).transpose()
}

pub async fn update_monitored_host_remark(
    pool: &DbPool,
    host_id: &str,
    remark: &str,
) -> anyhow::Result<bool> {
    let host_id = canonical_uuid(host_id)?;
    let mut tx = database::begin_write(pool).await?;
    let updated = query("UPDATE monitored_hosts SET name=?2 WHERE host_id=?1")
        .bind(&host_id)
        .bind(remark)
        .execute(tx.connection())
        .await?
        .rows_affected();
    if updated == 0 {
        tx.rollback().await?;
        return Ok(false);
    }
    crate::infra::database::insert_audit_in_transaction(
        tx.connection(),
        "monitoring.instance.remark.update",
        &host_id,
        Some("administrator updated the server-owned instance remark"),
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

pub async fn delete_monitored_host(pool: &DbPool, host_id: &str) -> anyhow::Result<bool> {
    let host_id = canonical_uuid(host_id)?;
    let mut tx = database::begin_write(pool).await?;
    let exists = query("SELECT 1 FROM monitored_hosts WHERE host_id=?1")
        .bind(&host_id)
        .fetch_optional(tx.connection())
        .await?
        .is_some();
    if !exists {
        tx.rollback().await?;
        return Ok(false);
    }

    crate::infra::database::insert_audit_in_transaction(
        tx.connection(),
        "monitoring.instance.delete",
        &host_id,
        Some("host, report history, credentials, pairing requests and invites permanently deleted"),
    )
    .await?;
    query("DELETE FROM agent_pairing_requests WHERE requested_host_id=?1 OR instance_id=?1")
    .bind(&host_id)
    .execute(tx.connection())
    .await?;
    query("DELETE FROM agent_instance_invites WHERE instance_id=?1")
        .bind(&host_id)
        .execute(tx.connection())
        .await?;
    query("DELETE FROM monitored_hosts WHERE host_id=?1")
        .bind(&host_id)
        .execute(tx.connection())
        .await?;
    tx.commit().await?;
    Ok(true)
}

fn history_query_sql(has_from: bool, has_to: bool) -> String {
    let (range_predicate, limit_parameter) = match (has_from, has_to) {
        (false, false) => ("", "?2"),
        (true, false) => ("AND collected_at >= ?2", "?3"),
        (false, true) => ("AND collected_at <= ?2", "?3"),
        (true, true) => (
            "AND collected_at >= ?2 AND collected_at <= ?3",
            "?4",
        ),
    };
    format!(
        r#"
        WITH recent AS (
            SELECT host_id, report_id, collected_at, received_at, {plain_metrics}
            FROM agent_metric_reports
            WHERE host_id = ?1
              {range_predicate}
            ORDER BY collected_at DESC, report_id DESC
            LIMIT {limit_parameter}
        )
        SELECT r.report_id, r.collected_at, r.received_at, {metrics}
        FROM (SELECT host_id FROM monitored_hosts WHERE host_id = ?1) h
        LEFT JOIN recent r ON r.host_id = h.host_id
        ORDER BY r.collected_at DESC, r.report_id DESC
        "#,
        metrics = METRIC_COLUMNS
            .iter()
            .map(|column| format!("r.{column}"))
            .collect::<Vec<_>>()
            .join(","),
        plain_metrics = METRIC_COLUMNS.join(","),
    )
}

/// 查询历史曲线。返回 `None` 表示主机不存在（供上层返回 404）。
///
/// 存在性判断与数据查询合并为**一次往返**：限量 CTE 与目标主机做 LEFT JOIN，使得
///   * 主机不存在 → 0 行；
///   * 主机存在但无历史 → 1 行，且 `report_id` 为 NULL；
///   * 主机存在且有历史 → N 行。
///
/// 先调 `get_monitored_host` 判存在、再查历史是更直白的写法，但那不仅多一次往返，
/// 更糟的是存在性检查会连带把完整的 `latest_report` JSON 读出来——仅仅为了判断
/// 一行是否存在。
pub async fn monitoring_history(
    pool: &DbPool,
    host_id: &str,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    limit: i64,
) -> anyhow::Result<Option<Vec<StoredHistoryPoint>>> {
    let host_id = canonical_uuid(host_id)?;
    let from = from.map(database::to_epoch_micros);
    let to = to.map(database::to_epoch_micros);
    // 只读摘要数值列。取出并反序列化整份 payload 的代价是 limit 上限 1000 × 单份
    // 30-50KB，而响应里真正需要的只有下面这几个标量。
    //
    // 走 idx_agent_metric_reports_host_collected_at
    // (host_id, collected_at DESC, report_id)，它完全覆盖了 WHERE 与 ORDER BY。
    // 若把 9 个指标列都放进索引可以减少回表，但会明显扩大每次上报必须
    // 更新的索引；实测读取无收益而写入慢 37%，因此不建立覆盖索引。
    let statement = history_query_sql(from.is_some(), to.is_some());
    let statement = query(&statement).bind(host_id);
    let statement = match (from, to) {
        (None, None) => statement.bind(limit),
        (Some(from), None) => statement.bind(from).bind(limit),
        (None, Some(to)) => statement.bind(to).bind(limit),
        (Some(from), Some(to)) => statement.bind(from).bind(to).bind(limit),
    };
    let rows = statement.fetch_all(pool).await?;

    // 0 行 = 主机不存在。
    if rows.is_empty() {
        return Ok(None);
    }
    let mut points = rows
        .into_iter()
        // 主机存在但无历史时，LEFT JOIN 会补出一行全 NULL 的占位，跳过即可。
        .filter_map(|row| {
            let report_id: Option<String> = match row.try_get("report_id") {
                Ok(value) => value,
                Err(error) => return Some(Err(error.into())),
            };
            let report_id = report_id?;
            Some((|| {
                Ok(StoredHistoryPoint {
                    report_id,
                    collected_at: timestamp(&row, "collected_at")?,
                    received_at: timestamp(&row, "received_at")?,
                    metrics: metrics_from_row(&row)?,
                })
            })())
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    points.reverse();
    Ok(Some(points))
}
