import { formatBytes, formatBytesPerSecond } from "../../../shared/lib/format";
import { Metric } from "../../../shared/components/ui";
import {
  formatMetric,
  formatPercent,
  formatTemperature,
  isNumber,
  metricTone,
  NA,
  sumNullable,
} from "../model";
import type { MonitoringAgentReport, MonitoringHostSummary } from "../types";

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
