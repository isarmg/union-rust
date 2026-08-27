import { Metric } from "../../../shared/components/ui";
import { formatBytesPerSecond } from "../../../shared/lib/format";
import {
  formatMetric,
  formatPercent,
  formatTemperature,
  historyValues,
  latestHistoryValue,
  metricTone,
  NA,
  sumNullable,
} from "../model";
import type { MonitoringHistoryPoint } from "../types";

export function HistoryMetrics({ points }: { points: MonitoringHistoryPoint[] }) {
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
    <div className="sarmg-grid metric-grid">
      <Metric label="CPU" value={formatPercent(latestHistoryValue(cpu))} detail={detail} tone={metricTone(latestHistoryValue(cpu))} sparkData={cpu} sparkMax={100} />
      <Metric label="内存" value={formatPercent(latestHistoryValue(memory))} detail={detail} tone={metricTone(latestHistoryValue(memory))} sparkData={memory} sparkMax={100} sparkColor="var(--warn)" />
      <Metric label="GPU" value={formatPercent(latestHistoryValue(gpu))} detail={detail} tone={metricTone(latestHistoryValue(gpu))} sparkData={gpu} sparkMax={100} sparkColor="var(--accent)" />
      <Metric label="网络" value={formatMetric(latestHistoryValue(network), formatBytesPerSecond)} detail={detail} tone="neutral" sparkData={network} sparkColor="var(--good)" />
      <Metric label="磁盘 I/O" value={formatMetric(latestHistoryValue(disk), formatBytesPerSecond)} detail={detail} tone="neutral" sparkData={disk} />
      <Metric label="温度" value={formatTemperature(latestHistoryValue(temperature))} detail={detail} tone={metricTone(latestHistoryValue(temperature), 80)} sparkData={temperature} sparkColor="var(--danger)" />
    </div>
  );
}
