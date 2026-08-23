import { useId, useState, type ReactNode } from "react";
import {
  Activity,
  CircuitBoard,
  Gauge,
  HardDrive,
  Network,
  ShieldCheck,
  Thermometer,
  X,
} from "lucide-react";
import { InlineNotice, LoadingBlock, StatusLed } from "../../../shared/components/ui";
import {
  addJsonU64,
  formatBytes,
  formatBytesPerSecond,
  formatDateTime,
  formatInteger,
  percent,
  subtractJsonU64,
} from "../../../shared/lib/format";
import { HistoryMetrics } from "./HistoryMetrics";
import {
  formatMetric,
  formatPercent,
  formatTemperature,
  isNumber,
  NA,
  statusMeta,
  sumNullable,
} from "../model";
import type {
  MonitoringAgentReport,
  MonitoringCapability,
  MonitoringGpuReport,
  MonitoringHistoryPoint,
  MonitoringHostSummary,
} from "../types";

type DetailSection = "overview" | "network" | "storage" | "gpu" | "temperature" | "capabilities" | "history";

const DETAIL_SECTIONS = [
  { key: "overview", label: "概览", Icon: Activity },
  { key: "network", label: "网络", Icon: Network },
  { key: "storage", label: "磁盘", Icon: HardDrive },
  { key: "gpu", label: "GPU", Icon: CircuitBoard },
  { key: "temperature", label: "温度", Icon: Thermometer },
  { key: "capabilities", label: "能力", Icon: ShieldCheck },
  { key: "history", label: "历史", Icon: Gauge },
] as const;

function DetailTable({
  title,
  columns,
  rows,
  emptyLabel = "暂无数据",
}: {
  title: string;
  columns: string[];
  rows: ReactNode[][];
  emptyLabel?: string;
}) {
  return (
    <section className="monitoring-table-section">
      <h3>{title}</h3>
      <div className="monitoring-table-scroll">
        <table className="monitoring-detail-table" aria-label={title}>
          <thead>
            <tr>{columns.map((column) => <th key={column} scope="col">{column}</th>)}</tr>
          </thead>
          <tbody>
            {rows.length ? rows.map((row, rowIndex) => (
              <tr key={rowIndex}>
                {row.map((cell, cellIndex) => <td key={cellIndex}>{cell}</td>)}
              </tr>
            )) : (
              <tr>
                <td className="monitoring-table-empty" colSpan={columns.length}>{emptyLabel}</td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </section>
  );
}

function OverviewTables({
  host,
  report,
}: {
  host: MonitoringHostSummary;
  report: MonitoringAgentReport | null | undefined;
}) {
  const memory = report?.system.memory;
  const network = sumNullable(
    host.network_received_bytes_per_second,
    host.network_transmitted_bytes_per_second,
  );
  const disk = sumNullable(
    host.disk_read_bytes_per_second,
    host.disk_written_bytes_per_second,
  );
  return (
    <div className="monitoring-table-stack">
      <DetailTable
        title="实例信息"
        columns={["字段", "值"]}
        rows={[
          ["名称", host.name || NA],
          ["状态", statusMeta(host.status).label],
          ["实例 ID", <span className="mono">{host.id}</span>],
          ["操作系统", [host.os, host.os_version].filter(Boolean).join(" ") || NA],
          ["内核", host.kernel_version || NA],
          ["架构", host.arch || NA],
          ["Agent 版本", host.agent_version || NA],
          ["注册时间", formatDateTime(host.registered_at)],
          ["最后上报", formatDateTime(host.last_seen_at)],
          ["最后采样", host.latest_collected_at ? formatDateTime(host.latest_collected_at) : NA],
        ]}
      />
      <DetailTable
        title="实时指标"
        columns={["项目", "当前值", "说明"]}
        rows={[
          [
            "CPU",
            formatPercent(host.cpu_usage_percent),
            report
              ? `${report.system.cpu.logical_count} 个逻辑核心${report.system.cpu.physical_count ? ` · ${report.system.cpu.physical_count} 个物理核心` : ""}`
              : NA,
          ],
          [
            "内存",
            formatPercent(host.memory_usage_percent),
            memory ? `${formatBytes(memory.used_bytes)} / ${formatBytes(memory.total_bytes)}` : NA,
          ],
          [
            "GPU",
            formatPercent(host.gpu_utilization_percent),
            isNumber(host.gpu_memory_usage_percent)
              ? `显存 ${formatPercent(host.gpu_memory_usage_percent)}`
              : NA,
          ],
          [
            "网络",
            formatMetric(network, formatBytesPerSecond),
            `收 ${formatMetric(host.network_received_bytes_per_second, formatBytesPerSecond)} · 发 ${formatMetric(host.network_transmitted_bytes_per_second, formatBytesPerSecond)}`,
          ],
          [
            "磁盘 I/O",
            formatMetric(disk, formatBytesPerSecond),
            `读 ${formatMetric(host.disk_read_bytes_per_second, formatBytesPerSecond)} · 写 ${formatMetric(host.disk_written_bytes_per_second, formatBytesPerSecond)}`,
          ],
          ["最高温度", formatTemperature(host.max_temperature_celsius), "当前可用传感器中的最高值"],
        ]}
      />
    </div>
  );
}

function NetworkTable({ report }: { report: MonitoringAgentReport | null | undefined }) {
  return (
    <DetailTable
      title="网络接口"
      columns={["接口", "接收速率", "发送速率", "累计接收", "累计发送", "收包", "发包", "错误"]}
      rows={(report?.system.networks ?? []).map((network) => [
        network.name || NA,
        formatBytesPerSecond(network.received_bytes_per_second),
        formatBytesPerSecond(network.transmitted_bytes_per_second),
        formatBytes(network.received_bytes_total),
        formatBytes(network.transmitted_bytes_total),
        formatInteger(network.packets_received_total),
        formatInteger(network.packets_transmitted_total),
        formatInteger(addJsonU64(network.receive_errors_total, network.transmit_errors_total)),
      ])}
      emptyLabel="暂无网络接口数据"
    />
  );
}

function StorageTable({ report }: { report: MonitoringAgentReport | null | undefined }) {
  return (
    <DetailTable
      title="磁盘与文件系统"
      columns={["设备", "挂载点", "文件系统", "已用 / 总量", "占用率", "读取", "写入", "模式"]}
      rows={(report?.system.disks ?? []).map((disk) => {
        const used = subtractJsonU64(disk.total_bytes, disk.available_bytes);
        return [
          disk.name || NA,
          disk.mount_point || NA,
          disk.file_system || NA,
          `${formatBytes(used)} / ${formatBytes(disk.total_bytes)}`,
          formatPercent(percent(used, disk.total_bytes)),
          formatBytesPerSecond(disk.read_bytes_per_second),
          formatBytesPerSecond(disk.written_bytes_per_second),
          disk.is_read_only ? "只读" : "读写",
        ];
      })}
      emptyLabel="暂无磁盘数据"
    />
  );
}

function gpuMemory(gpu: MonitoringGpuReport): string {
  return gpu.memory_used_bytes !== null && gpu.memory_total_bytes !== null
    ? `${formatBytes(gpu.memory_used_bytes)} / ${formatBytes(gpu.memory_total_bytes)}（${formatPercent(percent(gpu.memory_used_bytes, gpu.memory_total_bytes))}）`
    : NA;
}

function GpuTable({ report }: { report: MonitoringAgentReport | null | undefined }) {
  return (
    <DetailTable
      title="GPU"
      columns={["名称", "厂商", "占用", "显存", "温度", "功耗", "核心频率", "显存频率", "PCIe 收 / 发"]}
      rows={(report?.system.gpus ?? []).map((gpu) => [
        gpu.name || gpu.id || NA,
        gpu.vendor || NA,
        formatPercent(gpu.utilization_percent),
        gpuMemory(gpu),
        formatTemperature(gpu.temperature_celsius),
        formatMetric(gpu.power_watts, (value) => `${value.toFixed(1)} W`),
        formatMetric(gpu.core_clock_mhz, (value) => `${value.toFixed(0)} MHz`),
        formatMetric(gpu.memory_clock_mhz, (value) => `${value.toFixed(0)} MHz`),
        `${formatMetric(gpu.pcie_rx_bytes_per_second, formatBytesPerSecond)} / ${formatMetric(gpu.pcie_tx_bytes_per_second, formatBytesPerSecond)}`,
      ])}
      emptyLabel="暂无 GPU 数据"
    />
  );
}

function TemperatureTable({ report }: { report: MonitoringAgentReport | null | undefined }) {
  return (
    <DetailTable
      title="温度传感器"
      columns={["传感器", "当前", "上限", "临界", "来源"]}
      rows={(report?.system.temperatures ?? []).map((sensor) => [
        sensor.label || sensor.id || NA,
        formatTemperature(sensor.celsius),
        formatTemperature(sensor.max_celsius),
        formatTemperature(sensor.critical_celsius),
        sensor.source || NA,
      ])}
      emptyLabel="暂无温度传感器数据"
    />
  );
}

function CapabilityTable({ capabilities }: { capabilities: MonitoringCapability[] }) {
  return (
    <DetailTable
      title="采集能力"
      columns={["能力", "状态", "来源", "错误类型", "说明"]}
      rows={capabilities.map((capability) => [
        capability.name || NA,
        <span className="monitoring-capability-status">
          <StatusLed tone={capability.available ? "good" : "danger"} />
          {capability.available ? "支持" : "不可用"}
        </span>,
        capability.source || NA,
        capability.error_kind || NA,
        capability.message || NA,
      ])}
      emptyLabel="暂无采集能力数据"
    />
  );
}

export function MonitoringHostPanel({
  host,
  report,
  historyPoints,
  detailLoading,
  detailError,
  historyLoading,
  historyError,
  onClose,
}: {
  host: MonitoringHostSummary;
  report: MonitoringAgentReport | null | undefined;
  historyPoints: MonitoringHistoryPoint[];
  detailLoading: boolean;
  detailError: Error | null;
  historyLoading: boolean;
  historyError: Error | null;
  onClose: () => void;
}) {
  const [section, setSection] = useState<DetailSection>("overview");
  const tabsId = useId();

  return (
    <div className="sunshine-host-panel monitoring-host-panel">
      <div className="sunshine-panel-nav-row">
        <nav className="sunshine-subnav-inline" role="tablist" aria-label={`${host.name} 详情分类`}>
          {DETAIL_SECTIONS.map(({ key, label, Icon }) => (
            <button
              key={key}
              type="button"
              id={`${tabsId}-tab-${key}`}
              role="tab"
              aria-selected={section === key}
              aria-controls={`${tabsId}-panel-${key}`}
              className={section === key ? "sunshine-section-tab active" : "sunshine-section-tab"}
              onClick={() => setSection(key)}
            >
              <Icon size={18} /><strong>{label}</strong>
            </button>
          ))}
        </nav>
        <button
          type="button"
          className="icon-button sunshine-panel-close"
          aria-label="关闭详情面板"
          title="关闭"
          autoFocus
          onClick={onClose}
        >
          <X size={18} aria-hidden="true" />
        </button>
      </div>
      <div
        className="monitoring-detail-tabpanel"
        role="tabpanel"
        id={`${tabsId}-panel-${section}`}
        aria-labelledby={`${tabsId}-tab-${section}`}
      >
        {section !== "history" && detailLoading ? <LoadingBlock label="正在读取主机详情" /> : null}
        {section !== "history" && detailError ? <InlineNotice tone="danger" text={detailError.message} /> : null}
        {section === "overview" ? <OverviewTables host={host} report={report} /> : null}
        {section === "network" ? <NetworkTable report={report} /> : null}
        {section === "storage" ? <StorageTable report={report} /> : null}
        {section === "gpu" ? <GpuTable report={report} /> : null}
        {section === "temperature" ? <TemperatureTable report={report} /> : null}
        {section === "capabilities" ? (
          <CapabilityTable capabilities={report?.capabilities ?? host.capabilities} />
        ) : null}
        {section === "history" ? (
          <section className="monitoring-history-panel">
            {historyLoading ? <LoadingBlock label="正在读取历史指标" /> : null}
            {historyError ? <InlineNotice tone="danger" text={historyError.message} /> : null}
            <HistoryMetrics points={historyPoints} />
          </section>
        ) : null}
      </div>
    </div>
  );
}
