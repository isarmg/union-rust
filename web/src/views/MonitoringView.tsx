import { useEffect, useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Activity,
  CircuitBoard,
  Gauge,
  Copy,
  HardDrive,
  KeyRound,
  MonitorDot,
  Network,
  Plus,
  ShieldCheck,
  Thermometer,
  X,
} from "lucide-react";
import { api } from "../api";
import { queryKeys } from "../query-keys";
import type {
  AgentInstanceSummary,
  CreatedAgentInstance,
  MonitoringAgentReport,
  MonitoringCapability,
  MonitoringGpuReport,
  MonitoringHistoryPoint,
  MonitoringHostSummary,
} from "../types";
import { formatBytes, formatBytesPerSecond, formatDateTime, percent } from "../utils";
import {
  ActionButton,
  CardActions,
  CardInner,
  CardRow,
  InlineNotice,
  LoadingBlock,
  Metric,
  MutationError,
  ProgressBar,
  SectionHeader,
  StatusLed,
  TickerText,
  TruncatedText,
} from "../components/ui";

const NA = "N/A";
const HOST_PAGE_SIZE = 20;

function isNumber(value: number | null | undefined): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function formatMetric(value: number | null | undefined, formatter: (value: number) => string): string {
  return isNumber(value) ? formatter(value) : NA;
}

function formatPercent(value: number | null | undefined): string {
  return formatMetric(value, (metric) => `${metric.toFixed(1)}%`);
}

function formatTemperature(value: number | null | undefined): string {
  return formatMetric(value, (metric) => `${metric.toFixed(1)} °C`);
}

function sumNullable(...values: Array<number | null | undefined>): number | null {
  const available = values.filter(isNumber);
  return available.length ? available.reduce((total, value) => total + value, 0) : null;
}

function metricTone(value: number | null | undefined, threshold = 85): "good" | "warn" | "neutral" {
  if (!isNumber(value)) return "neutral";
  return value >= threshold ? "warn" : "good";
}

function statusMeta(status: MonitoringHostSummary["status"]) {
  if (status === "online") return { label: "在线", tone: "good" as const };
  if (status === "stale") return { label: "数据过期", tone: "warn" as const };
  if (status === "revoked") return { label: "已撤销", tone: "danger" as const };
  return { label: "离线", tone: "danger" as const };
}

function HostCard({ host, selected, onSelect }: {
  host: MonitoringHostSummary;
  selected: boolean;
  onSelect: () => void;
}) {
  const status = statusMeta(host.status);
  const network = sumNullable(
    host.network_received_bytes_per_second,
    host.network_transmitted_bytes_per_second,
  );
  return (
    <button
      className={`content-card monitoring-host-card${selected ? " selected" : ""}`}
      type="button"
      onClick={onSelect}
      aria-pressed={selected}
      aria-label={`查看主机 ${host.name}`}
    >
      <CardInner>
        <CardRow label="主机">
          <TruncatedText grow><TickerText>{host.name || NA}</TickerText></TruncatedText>
          <span title={status.label}><StatusLed tone={status.tone} /></span>
        </CardRow>
        <CardRow label="状态">{status.label}</CardRow>
        <CardRow label="系统">
          <TruncatedText><TickerText>{[host.os, host.arch].filter(Boolean).join(" · ") || NA}</TickerText></TruncatedText>
        </CardRow>
        <CardRow label="CPU">{formatPercent(host.cpu_usage_percent)}</CardRow>
        <CardRow label="GPU">{formatPercent(host.gpu_utilization_percent)}</CardRow>
        <CardRow label="网络">{formatMetric(network, formatBytesPerSecond)}</CardRow>
      </CardInner>
    </button>
  );
}

function LiveMetrics({ host, report }: {
  host: MonitoringHostSummary;
  report: MonitoringAgentReport | null | undefined;
}) {
  const network = sumNullable(
    host.network_received_bytes_per_second,
    host.network_transmitted_bytes_per_second,
  );
  const disk = sumNullable(host.disk_read_bytes_per_second, host.disk_written_bytes_per_second);
  const memory = report?.system.memory;
  return (
    <div className="content-grid metric-grid">
      <Metric
        label="CPU"
        value={formatPercent(host.cpu_usage_percent)}
        detail={report ? `${report.system.cpu.logical_count} 个逻辑核心` : NA}
        tone={metricTone(host.cpu_usage_percent)}
      />
      <Metric
        label="内存"
        value={formatPercent(host.memory_usage_percent)}
        detail={memory ? `${formatBytes(memory.used_bytes)} / ${formatBytes(memory.total_bytes)}` : NA}
        tone={metricTone(host.memory_usage_percent)}
      />
      <Metric
        label="GPU"
        value={formatPercent(host.gpu_utilization_percent)}
        detail={isNumber(host.gpu_memory_usage_percent) ? `显存 ${formatPercent(host.gpu_memory_usage_percent)}` : NA}
        tone={metricTone(host.gpu_utilization_percent)}
      />
      <Metric
        label="网络"
        value={formatMetric(network, formatBytesPerSecond)}
        detail={`收 ${formatMetric(host.network_received_bytes_per_second, formatBytesPerSecond)}  发 ${formatMetric(host.network_transmitted_bytes_per_second, formatBytesPerSecond)}`}
        tone="neutral"
        title="收/发分别取占用最高的单个接口，而非所有接口求和（规避 bridge 等虚拟接口重复计数）"
      />
      <Metric
        label="磁盘 I/O"
        value={formatMetric(disk, formatBytesPerSecond)}
        detail={`读 ${formatMetric(host.disk_read_bytes_per_second, formatBytesPerSecond)}  写 ${formatMetric(host.disk_written_bytes_per_second, formatBytesPerSecond)}`}
        tone="neutral"
        title="读/写分别取占用最高的单个磁盘，而非所有磁盘求和"
      />
      <Metric
        label="温度"
        value={formatTemperature(host.max_temperature_celsius)}
        detail={isNumber(host.max_temperature_celsius) ? "当前最高温度" : NA}
        tone={metricTone(host.max_temperature_celsius, 80)}
      />
    </div>
  );
}

function NotAvailableCard({ label }: { label: string }) {
  return (
    <article className="content-card monitoring-detail-card">
      <CardInner><CardRow label={label}>{NA}</CardRow></CardInner>
    </article>
  );
}

function GpuCard({ gpu }: { gpu: MonitoringGpuReport }) {
  const memoryUsage = isNumber(gpu.memory_used_bytes) && isNumber(gpu.memory_total_bytes)
    ? percent(gpu.memory_used_bytes, gpu.memory_total_bytes)
    : null;
  return (
    <article className="content-card monitoring-detail-card">
      <CardInner>
        <CardRow label="GPU">
          <TruncatedText><TickerText>{gpu.name || gpu.id || NA}</TickerText></TruncatedText>
        </CardRow>
        <CardRow label="占用">{formatPercent(gpu.utilization_percent)}</CardRow>
        <CardRow label="显存">
          {isNumber(gpu.memory_used_bytes) && isNumber(gpu.memory_total_bytes)
            ? `${formatBytes(gpu.memory_used_bytes)} / ${formatBytes(gpu.memory_total_bytes)}`
            : NA}
        </CardRow>
        <CardRow label="显存率">{isNumber(memoryUsage) ? <ProgressBar value={memoryUsage} /> : NA}</CardRow>
        <CardRow label="温度">{formatTemperature(gpu.temperature_celsius)}</CardRow>
        <CardRow label="功耗">{formatMetric(gpu.power_watts, (value) => `${value.toFixed(1)} W`)}</CardRow>
      </CardInner>
    </article>
  );
}

function HardwareDetails({ report }: { report: MonitoringAgentReport | null | undefined }) {
  const system = report?.system;
  return (
    <>
      <section className="section-band">
        <SectionHeader icon={Network} title="网络接口" />
        <div className="content-grid">
          {system?.networks.length ? system.networks.map((network, index) => (
            <article className="content-card monitoring-detail-card" key={index}>
              <CardInner>
                <CardRow label="接口"><TruncatedText><TickerText>{network.name || NA}</TickerText></TruncatedText></CardRow>
                <CardRow label="接收">{formatBytesPerSecond(network.received_bytes_per_second)}</CardRow>
                <CardRow label="发送">{formatBytesPerSecond(network.transmitted_bytes_per_second)}</CardRow>
                <CardRow label="收包">{network.packets_received_total.toLocaleString()}</CardRow>
                <CardRow label="发包">{network.packets_transmitted_total.toLocaleString()}</CardRow>
                <CardRow label="错误">{(network.receive_errors_total + network.transmit_errors_total).toLocaleString()}</CardRow>
              </CardInner>
            </article>
          )) : <NotAvailableCard label="网络" />}
        </div>
      </section>

      <section className="section-band">
        <SectionHeader icon={HardDrive} title="磁盘与文件系统" />
        <div className="content-grid">
          {system?.disks.length ? system.disks.map((disk, index) => {
            const used = Math.max(0, disk.total_bytes - disk.available_bytes);
            return (
              <article className="content-card monitoring-detail-card" key={index}>
                <CardInner>
                  <CardRow label="设备"><TruncatedText><TickerText>{disk.name || NA}</TickerText></TruncatedText></CardRow>
                  <CardRow label="挂载"><TruncatedText><TickerText>{disk.mount_point || NA}</TickerText></TruncatedText></CardRow>
                  <CardRow label="占用"><ProgressBar value={percent(used, disk.total_bytes)} /></CardRow>
                  <CardRow label="容量">{`${formatBytes(used)} / ${formatBytes(disk.total_bytes)}`}</CardRow>
                  <CardRow label="吞吐">{formatBytesPerSecond(disk.read_bytes_per_second + disk.written_bytes_per_second)}</CardRow>
                  <CardRow label="模式">{disk.is_read_only ? "只读" : "读写"}</CardRow>
                </CardInner>
              </article>
            );
          }) : <NotAvailableCard label="磁盘" />}
        </div>
      </section>

      <section className="section-band">
        <SectionHeader icon={CircuitBoard} title="GPU" />
        <div className="content-grid">
          {system?.gpus.length ? system.gpus.map((gpu, index) => <GpuCard key={index} gpu={gpu} />) : <NotAvailableCard label="GPU" />}
        </div>
      </section>

      <section className="section-band">
        <SectionHeader icon={Thermometer} title="温度传感器" />
        <div className="content-grid">
          {system?.temperatures.length ? system.temperatures.map((sensor, index) => (
            <article className="content-card monitoring-detail-card" key={index}>
              <CardInner>
                <CardRow label="传感器"><TruncatedText><TickerText>{sensor.label || sensor.id || NA}</TickerText></TruncatedText></CardRow>
                <CardRow label="当前">{formatTemperature(sensor.celsius)}</CardRow>
                <CardRow label="上限">{formatTemperature(sensor.max_celsius)}</CardRow>
                <CardRow label="临界">{formatTemperature(sensor.critical_celsius)}</CardRow>
                <CardRow label="来源"><TruncatedText><TickerText>{sensor.source || NA}</TickerText></TruncatedText></CardRow>
              </CardInner>
            </article>
          )) : <NotAvailableCard label="温度" />}
        </div>
      </section>
    </>
  );
}

function CapabilityCard({ capability }: { capability: MonitoringCapability }) {
  const detail = capability.message || capability.error_kind || NA;
  return (
    <article className="content-card monitoring-detail-card">
      <CardInner>
        <CardRow label="能力">
          <TruncatedText grow><TickerText>{capability.name || NA}</TickerText></TruncatedText>
          <StatusLed tone={capability.available ? "good" : "danger"} />
        </CardRow>
        <CardRow label="状态">{capability.available ? "支持" : "不可用"}</CardRow>
        <CardRow label="来源"><TruncatedText><TickerText>{capability.source || NA}</TickerText></TruncatedText></CardRow>
        <CardRow label="说明" span={3}><TruncatedText muted>{detail}</TruncatedText></CardRow>
      </CardInner>
    </article>
  );
}

function CapabilityDetails({ capabilities }: { capabilities: MonitoringCapability[] }) {
  return (
    <section className="section-band">
      <SectionHeader icon={ShieldCheck} title="采集能力" description="不可采集与真实值为 0 含义不同；缺失指标统一显示 N/A。" />
      <div className="content-grid">
        {capabilities.length
          // key 用下标而非 capability.name：name 由 Agent 提供，服务端不保证唯一，
          // 重名会让 React 复用到错误的节点。这里的列表顺序本就来自上报顺序、
          // 不做增删排序，下标是稳定的。
          ? capabilities.map((capability, index) => <CapabilityCard key={index} capability={capability} />)
          : <NotAvailableCard label="能力" />}
      </div>
    </section>
  );
}

export function historyValues(
  points: MonitoringHistoryPoint[],
  read: (point: MonitoringHistoryPoint) => number | null,
): Array<number | null> {
  return points.map((point) => {
    const value = read(point);
    return isNumber(value) ? value : null;
  });
}

export function latestHistoryValue(values: Array<number | null>): number | null {
  return values.length ? values[values.length - 1] : null;
}

function HistoryMetrics({ points }: { points: MonitoringHistoryPoint[] }) {
  const cpu = historyValues(points, (point) => point.cpu_usage_percent);
  const memory = historyValues(points, (point) => point.memory_usage_percent);
  const gpu = historyValues(points, (point) => point.gpu_utilization_percent);
  const temperature = historyValues(points, (point) => point.max_temperature_celsius);
  const network = historyValues(points, (point) => sumNullable(
    point.network_received_bytes_per_second,
    point.network_transmitted_bytes_per_second,
  ));
  const disk = historyValues(points, (point) => sumNullable(
    point.disk_read_bytes_per_second,
    point.disk_written_bytes_per_second,
  ));
  const detail = points.length ? `${points.length} 个采样点` : NA;
  return (
    <div className="content-grid metric-grid">
      <Metric label="CPU" value={formatPercent(latestHistoryValue(cpu))} detail={detail} tone={metricTone(latestHistoryValue(cpu))} sparkData={cpu} sparkMax={100} />
      <Metric label="内存" value={formatPercent(latestHistoryValue(memory))} detail={detail} tone={metricTone(latestHistoryValue(memory))} sparkData={memory} sparkMax={100} sparkColor="var(--warn)" />
      <Metric label="GPU" value={formatPercent(latestHistoryValue(gpu))} detail={detail} tone={metricTone(latestHistoryValue(gpu))} sparkData={gpu} sparkMax={100} sparkColor="var(--accent)" />
      <Metric label="网络" value={formatMetric(latestHistoryValue(network), formatBytesPerSecond)} detail={detail} tone="neutral" sparkData={network} sparkColor="var(--good)" />
      <Metric label="磁盘 I/O" value={formatMetric(latestHistoryValue(disk), formatBytesPerSecond)} detail={detail} tone="neutral" sparkData={disk} />
      <Metric label="温度" value={formatTemperature(latestHistoryValue(temperature))} detail={detail} tone={metricTone(latestHistoryValue(temperature), 80)} sparkData={temperature} sparkColor="var(--danger)" />
    </div>
  );
}

function ActivationCodePanel({ created, onClose }: {
  created: CreatedAgentInstance;
  onClose: () => void;
}) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="agent-created-instance">
      <InlineNotice
        tone="warn"
        text={agentAuthorizationKeyGuidance}
      />
      <dl className="agent-instance-details">
        <div><dt>状态</dt><dd><StatusLed tone="warn" /> 待激活</dd></div>
        <div><dt>显示名称</dt><dd>{created.display_name}</dd></div>
        <div><dt>一次性授权密钥</dt><dd className="agent-activation-code">{created.activation_code}</dd></div>
        {created.instance_id ? <div><dt>实例 ID</dt><dd className="mono">{created.instance_id}</dd></div> : null}
        <div><dt>到期时间</dt><dd>{formatDateTime(created.expires_at)}</dd></div>
      </dl>
      <div className="button-row">
        <ActionButton
          icon={Copy}
          label={copied ? "已复制授权密钥" : "复制授权密钥"}
          onClick={() => {
            void navigator.clipboard.writeText(created.activation_code)
              .then(() => setCopied(true))
              .catch(() => setCopied(false));
          }}
        />
        <ActionButton icon={X} label="关闭并清除授权密钥" onClick={onClose} />
      </div>
    </div>
  );
}

/** 主机凭据管理：浏览器只接触一次性授权密钥，不再显示长期 Agent 令牌。 */
function HostRegistration({ host }: { host: MonitoringHostSummary }) {
  const queryClient = useQueryClient();
  const [created, setCreated] = useState<CreatedAgentInstance | null>(null);

  const rePairMutation = useMutation({
    mutationFn: () => api.monitoringCreateAgentInstance(host.name, 15, host.id),
    onSuccess: async (result) => {
      setCreated(result);
      await queryClient.invalidateQueries({ queryKey: queryKeys.monitoring.agentInstances });
    },
  });
  const revokeMutation = useMutation({
    mutationFn: () => api.monitoringRevokeHost(host.id),
    onSuccess: async () => {
      setCreated(null);
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: queryKeys.monitoring.hosts }),
        queryClient.invalidateQueries({ queryKey: queryKeys.monitoring.host(host.id) }),
      ]);
    },
  });
  return (
    <section className="section-band">
      <SectionHeader
        icon={ShieldCheck}
        title="注册管理"
        description={host.lifecycle_status === "revoked"
          ? "该实例已撤销；可为同一实例重新配对，恢复后会继续使用原有历史。"
          : "重新配对只生成短时一次性授权密钥；撤销凭据会让 Agent 停止上报，同时保留主机和全部历史。"}
      />
      <CardActions>
        <ActionButton
          icon={KeyRound}
          label="重新配对"
          busy={rePairMutation.isPending}
          onClick={() => rePairMutation.mutate()}
        />
        {host.lifecycle_status === "active" ? (
          <ActionButton
            icon={X}
            label="撤销 Agent"
            tone="danger"
            busy={revokeMutation.isPending}
            onClick={() => window.confirm(
              `撤销 "${host.name}" 的 Agent 凭据？\n\n` +
                "Agent 将无法继续上报，但主机记录和历史数据会保留。之后可通过“重新配对”恢复。",
            ) && revokeMutation.mutate()}
          />
        ) : null}
      </CardActions>
      <MutationError mutation={rePairMutation} />
      <MutationError mutation={revokeMutation} />
      {created ? <ActivationCodePanel created={created} onClose={() => setCreated(null)} /> : null}
    </section>
  );
}

export function pendingAgentInstances(instances: AgentInstanceSummary[]): AgentInstanceSummary[] {
  return instances.filter((instance) => instance.status === "pending");
}

export const agentAuthorizationKeyGuidance =
  "授权密钥只在本次创建后显示。Windows 请在 Agent 托盘的“本地配置”页填写服务器地址和此密钥；CLI 配对请在 Agent 打开的激活页确认。";

function AgentInstances({ activeHostIds }: { activeHostIds: ReadonlySet<string> }) {
  const queryClient = useQueryClient();
  const [displayName, setDisplayName] = useState("新监控主机");
  const [expiresInMinutes, setExpiresInMinutes] = useState(15);
  const [created, setCreated] = useState<CreatedAgentInstance | null>(null);
  const [creationOutcome, setCreationOutcome] = useState<{
    displayName: string;
    status: AgentInstanceSummary["status"];
  } | null>(null);
  const instancesQuery = useQuery({
    queryKey: queryKeys.monitoring.agentInstances,
    queryFn: api.monitoringAgentInstances,
    refetchInterval: 10_000,
  });
  const createMutation = useMutation({
    mutationFn: () => api.monitoringCreateAgentInstance(displayName.trim(), expiresInMinutes),
    onSuccess: async (result) => {
      setCreationOutcome(null);
      setCreated(result);
      await queryClient.invalidateQueries({ queryKey: queryKeys.monitoring.agentInstances });
    },
  });
  const cancelMutation = useMutation({
    mutationFn: api.monitoringCancelAgentInstance,
    onSuccess: async (_result, requestId) => {
      if (created?.request_id === requestId) setCreated(null);
      await queryClient.invalidateQueries({ queryKey: queryKeys.monitoring.agentInstances });
    },
  });
  const pending = pendingAgentInstances(instancesQuery.data ?? []);
  const refreshedCreated = created
    ? instancesQuery.data?.find((instance) => instance.request_id === created.request_id)
    : undefined;
  const createdStatus = created?.instance_id && activeHostIds.has(created.instance_id)
    ? "active"
    : (refreshedCreated?.status ?? created?.status);

  useEffect(() => {
    if (!created || !createdStatus || createdStatus === "pending") return;
    setCreationOutcome({ displayName: created.display_name, status: createdStatus });
    // Drop the one-time plaintext credential as soon as it can no longer be used.
    setCreated(null);
  }, [created, createdStatus]);

  return (
    <section className="section-band agent-instances">
      <SectionHeader
        icon={Plus}
        title="创建 Agent"
        description="请先通过所属平台的软件分发渠道安装 UnionC Agent。Windows 可在 Agent 本地配置页填写 UnionC 服务器地址和管理台生成的一次性授权密钥；CLI 配对会打开激活页。管理中心不托管安装包，也不生成系统命令。"
      />
      <div className="agent-instance-form">
        <label>
          <span>显示名称</span>
          <input
            value={displayName}
            maxLength={128}
            onChange={(event) => setDisplayName(event.target.value)}
            placeholder="例如 办公室工作站"
          />
        </label>
        <label>
          <span>授权密钥有效期</span>
          <select
            value={expiresInMinutes}
            onChange={(event) => setExpiresInMinutes(Number(event.target.value))}
          >
            <option value={15}>15 分钟（推荐）</option>
            <option value={30}>30 分钟</option>
            <option value={60}>1 小时</option>
            <option value={1440}>24 小时</option>
          </select>
        </label>
        <ActionButton
          icon={Plus}
          label="创建 Agent"
          busy={createMutation.isPending}
          disabled={!displayName.trim()}
          onClick={() => createMutation.mutate()}
        />
      </div>
      <MutationError mutation={createMutation} />
      <MutationError mutation={cancelMutation} />
      {created && createdStatus === "pending" ? (
        <ActivationCodePanel created={created} onClose={() => setCreated(null)} />
      ) : null}
      {creationOutcome?.status === "active" ? (
        <div className="agent-instance-activated" role="status">
          <ShieldCheck size={18} aria-hidden="true" />
          <span>{creationOutcome.displayName} 已激活，并已转入下方主机列表。</span>
          <ActionButton icon={X} label="关闭" onClick={() => setCreationOutcome(null)} />
        </div>
      ) : null}
      {creationOutcome && creationOutcome.status !== "active" ? (
        <InlineNotice tone="warn" text={`配对邀请已${creationOutcome.status === "expired" ? "过期" : "取消"}，授权密钥已从内存和页面清除。`} />
      ) : null}
      <div className="agent-pending-instances">
        <span className="muted-inline">待激活项</span>
        {instancesQuery.isLoading ? <LoadingBlock label="正在读取待激活项" /> : null}
        {instancesQuery.error ? <InlineNotice tone="danger" text={instancesQuery.error.message} /> : null}
        {!instancesQuery.isLoading && !instancesQuery.error && !pending.length ? (
          <div className="empty-state">暂无待激活 Agent</div>
        ) : null}
        {pending.map((instance) => (
          <div className="agent-pending-instance" key={instance.request_id}>
            <div>
              <strong>{instance.display_name}</strong>
              <span className="muted-inline">
                待激活 · 到期 {formatDateTime(instance.expires_at)}
              </span>
              <span className="mono">请求 {instance.request_id}</span>
            </div>
            <div className="agent-pending-instance-actions">
              <ActionButton
                icon={X}
                label="取消"
                busy={cancelMutation.isPending && cancelMutation.variables === instance.request_id}
                onClick={() => window.confirm(
                  `取消 "${instance.display_name}" 的待激活邀请？\n\n此授权密钥将立即失效。`,
                ) && cancelMutation.mutate(instance.request_id)}
              />
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}

export function MonitoringView() {
  const [offset, setOffset] = useState(0);
  const [preferredHostId, setPreferredHostId] = useState<string | null>(null);
  const hostsQuery = useQuery({
    queryKey: queryKeys.monitoring.hostPage(HOST_PAGE_SIZE, offset),
    queryFn: () => api.monitoringHosts(HOST_PAGE_SIZE, offset),
    refetchInterval: 10_000,
  });
  const hosts = useMemo(() => hostsQuery.data?.hosts ?? [], [hostsQuery.data?.hosts]);
  const activeHostIds = useMemo(
    () => new Set(hosts.filter((host) => host.lifecycle_status === "active").map((host) => host.id)),
    [hosts],
  );
  // 服务端对主机列表分页。截断必须让用户看见——否则"少了几台机器"会被当成
  // Agent 掉线去排查，而真实原因只是没翻页。
  const total = hostsQuery.data ? hostsQuery.data.total : 0;
  const hasPreviousPage = offset > 0;
  const hasNextPage = offset + hosts.length < total;

  useEffect(() => {
    if (total > 0 && offset >= total) {
      setOffset(Math.floor((total - 1) / HOST_PAGE_SIZE) * HOST_PAGE_SIZE);
    }
  }, [offset, total]);

  const selectedHostId = preferredHostId && hosts.some((host) => host.id === preferredHostId)
    ? preferredHostId
    : (hosts[0]?.id ?? null);

  const detailQuery = useQuery({
    queryKey: queryKeys.monitoring.host(selectedHostId ?? ""),
    queryFn: () => api.monitoringHost(selectedHostId!),
    enabled: Boolean(selectedHostId),
    refetchInterval: 10_000,
  });
  const historyQuery = useQuery({
    queryKey: queryKeys.monitoring.history(selectedHostId ?? ""),
    queryFn: () => api.monitoringHistory(selectedHostId!),
    enabled: Boolean(selectedHostId),
    refetchInterval: 30_000,
  });
  const selectedSummary = hosts.find((host) => host.id === selectedHostId);
  const selectedHost = detailQuery.data?.host ?? selectedSummary;
  const latest = detailQuery.data?.latest;
  const historyPoints = useMemo(
    () => [...(historyQuery.data?.points ?? [])].sort((left, right) => left.collected_at.localeCompare(right.collected_at)),
    [historyQuery.data],
  );
  const onlineCount = hosts.filter((host) => host.status === "online").length;

  return (
    <section className="view-stack monitoring-view">
      <AgentInstances activeHostIds={activeHostIds} />
      <section className="section-band">
        <SectionHeader icon={MonitorDot} title="主机监控" description={`只读采集 · ${onlineCount}/${hosts.length} 台在线`} />
        {hostsQuery.isLoading ? <LoadingBlock label="正在读取主机状态" /> : null}
        {hostsQuery.error ? <InlineNotice tone="danger" text={hostsQuery.error.message} /> : null}
        {!hostsQuery.isLoading && !hostsQuery.error && !hosts.length ? <div className="empty-state">暂无 Agent 上报数据</div> : null}
        {total > HOST_PAGE_SIZE ? (
          <div className="button-row" aria-label="监控主机分页">
            <button className="card-action-button" type="button" disabled={!hasPreviousPage}
              onClick={() => setOffset(current => Math.max(0, current - HOST_PAGE_SIZE))}>
              上一页
            </button>
            <span className="muted-inline">
              {offset + 1}–{Math.min(offset + hosts.length, total)} / {total}
            </span>
            <button className="card-action-button" type="button" disabled={!hasNextPage}
              onClick={() => setOffset(current => current + HOST_PAGE_SIZE)}>
              下一页
            </button>
          </div>
        ) : null}
        <div className="content-grid monitoring-host-grid">
          {hosts.map((host) => (
            <HostCard key={host.id} host={host} selected={host.id === selectedHostId} onSelect={() => setPreferredHostId(host.id)} />
          ))}
        </div>
      </section>

      {selectedHost ? (
        <>
          <section className="section-band">
            <SectionHeader
              icon={Activity}
              title={selectedHost.name || "主机详情"}
              description={`${statusMeta(selectedHost.status).label} · ${selectedHost.os || NA} ${selectedHost.os_version ?? ""} · Agent ${selectedHost.agent_version || NA} · 最后上报 ${formatDateTime(selectedHost.last_seen_at)}`}
            />
            {detailQuery.error ? <InlineNotice tone="danger" text={detailQuery.error.message} /> : null}
            {detailQuery.isLoading ? <LoadingBlock label="正在读取实时指标" /> : null}
            <LiveMetrics host={selectedHost} report={latest} />
          </section>
          <HardwareDetails report={latest} />
          <CapabilityDetails capabilities={selectedHost.capabilities} />
          <section className="section-band">
            <SectionHeader icon={Gauge} title="历史趋势" description="最近采样点；页面只读取状态，不会向主机发送控制命令。" />
            {historyQuery.isLoading ? <LoadingBlock label="正在读取历史指标" /> : null}
            {historyQuery.error ? <InlineNotice tone="danger" text={historyQuery.error.message} /> : null}
            <HistoryMetrics points={historyPoints} />
          </section>
          <HostRegistration key={selectedHost.id} host={selectedHost} />
        </>
      ) : null}
    </section>
  );
}
