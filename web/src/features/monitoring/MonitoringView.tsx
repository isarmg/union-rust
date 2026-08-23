import { useEffect, useMemo, useState } from "react";
import { Activity, Gauge, MonitorDot } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { InlineNotice, LoadingBlock, SectionHeader } from "../../shared/components/ui";
import { formatDateTime } from "../../shared/lib/format";
import { monitoringApi as api } from "./api";
import { AgentInstances, HostRegistration } from "./components/AgentInstances";
import { CapabilityDetails, HardwareDetails } from "./components/HardwareDetails";
import { HistoryMetrics } from "./components/HistoryMetrics";
import { LiveMetrics } from "./components/HostSummary";
import { NA, statusMeta } from "./model";
import { monitoringQueryKeys as queryKeys } from "./queryKeys";

export {
  agentAuthorizationKeyGuidance,
  historyValues,
  latestHistoryValue,
  pendingAgentInstances,
} from "./model";

const HOST_PAGE_SIZE = 20;

export function MonitoringView({
  addTrigger = 0,
  onAddTriggerHandled,
}: {
  addTrigger?: number;
  onAddTriggerHandled?: (trigger: number) => void;
}) {
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
  const total = hostsQuery.data?.total ?? 0;
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
    () => [...(historyQuery.data?.points ?? [])]
      .sort((left, right) => left.collected_at.localeCompare(right.collected_at)),
    [historyQuery.data],
  );
  return (
    <section className="view-stack monitoring-view">
      <AgentInstances
        activeHostIds={activeHostIds}
        addTrigger={addTrigger}
        onAddTriggerHandled={onAddTriggerHandled}
      />
      <section className="section-band">
        <SectionHeader icon={MonitorDot} title="主机监控" />
        {hostsQuery.isLoading ? <LoadingBlock label="正在读取主机状态" /> : null}
        {hostsQuery.error ? <InlineNotice tone="danger" text={hostsQuery.error.message} /> : null}
        {total > HOST_PAGE_SIZE ? (
          <div className="button-row" aria-label="监控主机分页">
            <button
              className="card-action-button"
              type="button"
              disabled={!hasPreviousPage}
              onClick={() => setOffset((current) => Math.max(0, current - HOST_PAGE_SIZE))}
            >上一页</button>
            <span className="muted-inline">
              {offset + 1}–{Math.min(offset + hosts.length, total)} / {total}
            </span>
            <button
              className="card-action-button"
              type="button"
              disabled={!hasNextPage}
              onClick={() => setOffset((current) => current + HOST_PAGE_SIZE)}
            >下一页</button>
          </div>
        ) : null}
        <div className="content-grid monitoring-host-grid">
          {hosts.map((host) => (
            <HostRegistration
              key={host.id}
              host={host}
              selected={host.id === selectedHostId}
              onSelect={() => setPreferredHostId(host.id)}
              onDeleted={() => {
                if (preferredHostId === host.id) setPreferredHostId(null);
              }}
            />
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
        </>
      ) : null}
    </section>
  );
}
