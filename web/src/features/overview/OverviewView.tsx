import { HardDrive, MonitorDot, Server } from "lucide-react";
import type { ServiceStatus, SystemResources } from "./types";
import {
  CardInner,
  CardRow,
  LoadingBlock,
  Metric,
  ProgressBar,
  SectionHeader,
  TickerText,
  TruncatedText
} from "../../shared/components/ui";
import { ServiceCard } from "./ServiceCard";
import { formatBytes, formatBytesPerSecond, formatKib, percent } from "../../shared/lib/format";
import type { MetricHistory } from "../../app/hooks";

export function OverviewView({
  services,
  unhealthyCount,
  resources,
  history,
  loading
}: {
  services: ServiceStatus[];
  unhealthyCount: number;
  resources: SystemResources | undefined;
  history: MetricHistory;
  loading: boolean;
}) {
  const memoryPercent = resources
    ? percent(resources.memory_used_kib, resources.memory_total_kib)
    : 0;
  const healthyCount = services.filter((s) => s.healthy).length;

  return (
    <section className="view-stack">
      <section className="section-band metric-section">
        <SectionHeader icon={MonitorDot} title="监控" />
        <div className="content-grid metric-grid">
          <Metric
            label="服务"
            value={`${healthyCount}/${services.length}`}
            detail={unhealthyCount ? `${unhealthyCount} 项需要处理` : "全部可用"}
            tone={unhealthyCount ? "warn" : "good"}
          />
          <Metric
            label="CPU"
            value={resources ? `${resources.cpu_usage_percent.toFixed(1)}%` : "--"}
            tone={resources && resources.cpu_usage_percent > 80 ? "warn" : "good"}
            sparkData={history.cpu}
            sparkColor="var(--primary)"
            sparkMax={100}
          />
          <Metric
            label="内存"
            value={resources ? `${memoryPercent.toFixed(0)}%` : "--"}
            detail={
              resources
                ? `${formatKib(resources.memory_used_kib)} / ${formatKib(resources.memory_total_kib)}`
                : "等待资源数据"
            }
            tone={memoryPercent > 80 ? "warn" : "neutral"}
            sparkData={history.memory}
            sparkColor="var(--warn)"
            sparkMax={100}
          />
          <Metric
            label="网络"
            value={
              resources
                ? formatBytesPerSecond(resources.network.total_bytes_per_second)
                : "--"
            }
            tone="neutral"
            sparkData={history.network}
            sparkColor="var(--good)"
          />
          <Metric
            label="磁盘"
            value={
              resources
                ? formatBytesPerSecond(resources.disk_throughput.total_bytes_per_second)
                : "--"
            }
            detail={
              resources
                ? `读 ${formatBytesPerSecond(resources.disk_throughput.read_bytes_per_second)}  写 ${formatBytesPerSecond(resources.disk_throughput.write_bytes_per_second)}`
                : undefined
            }
            tone="neutral"
            sparkData={history.disk}
            sparkColor="var(--primary)"
          />
        </div>
      </section>

      <section className="section-band">
        <SectionHeader icon={Server} title="服务概览" />
        {loading ? <LoadingBlock label="正在读取服务状态" /> : null}
        <div className="content-grid service-grid">
          {services.map((service) => (
            <ServiceCard key={service.name} service={service} compact />
          ))}
        </div>
      </section>

      <section className="section-band">
        <SectionHeader icon={HardDrive} title="磁盘" />
        <div className="content-grid disk-list">
          {(resources ? resources.disks : []).map((disk) => {
            const used = disk.total_bytes - disk.available_bytes;
            return (
              <div className="content-card disk-card" key={`${disk.name}-${disk.mount_point}`}>
                <CardInner>
                  <CardRow label="设备">
                    <TruncatedText>
                      <TickerText>{disk.name || disk.mount_point}</TickerText>
                    </TruncatedText>
                  </CardRow>
                  <CardRow label="挂载">
                    <TruncatedText>
                      <TickerText>{disk.mount_point}</TickerText>
                    </TruncatedText>
                  </CardRow>
                  <CardRow label="占用">
                    <ProgressBar value={percent(used, disk.total_bytes)} />
                  </CardRow>
                  <CardRow label="容量">
                    <TruncatedText muted>
                      {formatBytes(used)} / {formatBytes(disk.total_bytes)}
                    </TruncatedText>
                  </CardRow>
                </CardInner>
              </div>
            );
          })}
        </div>
      </section>
    </section>
  );
}
