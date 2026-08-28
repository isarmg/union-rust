import { MonitorDot, Server } from "lucide-react";
import type { ServiceStatus } from "./types";
import { LoadingBlock, Metric, SectionHeader } from "../../shared/components/ui";
import { ServiceCard } from "./ServiceCard";

export function OverviewView({
  services,
  unhealthyCount,
  loading,
}: {
  services: ServiceStatus[];
  unhealthyCount: number;
  loading: boolean;
}) {
  const healthyCount = services.filter((service) => service.healthy).length;
  const runningCount = services.filter((service) => service.runtime_state === "available").length;

  return (
    <section className="view-stack">
      <section className="section-band metric-section">
        <SectionHeader icon={MonitorDot} title="平台状态" />
        <div className="sarmg-grid metric-grid">
          <Metric
            label="可用模块"
            value={`${healthyCount}/${services.length}`}
            detail={unhealthyCount ? `${unhealthyCount} 项需要处理` : "全部可用"}
            tone={unhealthyCount ? "warn" : "good"}
          />
          <Metric
            label="运行进程"
            value={`${runningCount}`}
            detail="由 Union 生命周期管理器监管"
            tone={runningCount === services.length ? "good" : "neutral"}
          />
        </div>
      </section>

      <section className="section-band">
        <SectionHeader icon={Server} title="模块服务" />
        {loading ? <LoadingBlock label="正在读取模块状态" /> : null}
        <div className="sarmg-grid service-grid">
          {services.map((service) => (
            <ServiceCard key={service.name} service={service} compact />
          ))}
        </div>
      </section>
    </section>
  );
}
