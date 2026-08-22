import { formatBytes, formatBytesPerSecond } from "../../../shared/lib/format";
import {
  CardInner,
  CardRow,
  Metric,
  StatusLed,
  TickerText,
  TruncatedText,
} from "../../../shared/components/ui";
import {
  formatMetric,
  formatPercent,
  formatTemperature,
  isNumber,
  metricTone,
  NA,
  statusMeta,
  sumNullable,
} from "../model";
import type { MonitoringAgentReport, MonitoringHostSummary } from "../types";

export function MonitoringHostCard({ host, selected, onSelect }: {
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

export function LiveMetrics({ host, report }: {
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
