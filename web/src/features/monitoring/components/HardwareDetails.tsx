import { CircuitBoard, HardDrive, Network, ShieldCheck, Thermometer } from "lucide-react";
import { formatBytes, formatBytesPerSecond, percent } from "../../../shared/lib/format";
import {
  CardInner,
  CardRow,
  ProgressBar,
  SectionHeader,
  StatusLed,
  TickerText,
  TruncatedText,
} from "../../../shared/components/ui";
import { formatMetric, formatPercent, formatTemperature, isNumber, NA } from "../model";
import type { MonitoringAgentReport, MonitoringCapability, MonitoringGpuReport } from "../types";

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

export function HardwareDetails({ report }: { report: MonitoringAgentReport | null | undefined }) {
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
          {system?.gpus.length
            ? system.gpus.map((gpu, index) => <GpuCard key={index} gpu={gpu} />)
            : <NotAvailableCard label="GPU" />}
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

export function CapabilityDetails({ capabilities }: { capabilities: MonitoringCapability[] }) {
  return (
    <section className="section-band">
      <SectionHeader icon={ShieldCheck} title="采集能力" description="不可采集与真实值为 0 含义不同；缺失指标统一显示 N/A。" />
      <div className="content-grid">
        {capabilities.length
          ? capabilities.map((capability, index) => <CapabilityCard key={index} capability={capability} />)
          : <NotAvailableCard label="能力" />}
      </div>
    </section>
  );
}
