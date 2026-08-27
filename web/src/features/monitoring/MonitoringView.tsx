import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { MonitorDot } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { InlineNotice, LoadingBlock, SectionHeader } from "../../shared/components/ui";
import { adjacentPanelLayout } from "../../shared/lib/adjacentPanel";
import { monitoringApi as api } from "./api";
import { AgentInstances, HostRegistration } from "./components/AgentInstances";
import { MonitoringHostPanel } from "./components/HardwareDetails";
import { monitoringQueryKeys as queryKeys } from "./queryKeys";
import "./monitoring.css";

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
  const [openHostId, setOpenHostId] = useState<string | null>(null);
  const hostGridRef = useRef<HTMLDivElement>(null);
  const detailPanelRef = useRef<HTMLElement>(null);
  const detailPanelOpenerRef = useRef<HTMLButtonElement | null>(null);
  const restoreDetailFocusRef = useRef(false);
  const hostsQuery = useQuery({
    queryKey: queryKeys.monitoring.hostPage(HOST_PAGE_SIZE, offset),
    queryFn: () => api.monitoringHosts(HOST_PAGE_SIZE, offset),
    refetchInterval: 10_000,
  });
  const hosts = useMemo(() => hostsQuery.data?.hosts ?? [], [hostsQuery.data?.hosts]);
  const activeHostIds = useMemo(
    () => new Set(hosts.map((host) => host.id)),
    [hosts],
  );
  const total = hostsQuery.data?.total ?? 0;
  const hasPreviousPage = offset > 0;
  const hasNextPage = offset + hosts.length < total;

  useEffect(() => {
    if (total > 0 && offset >= total) {
      setOffset(Math.floor((total - 1) / HOST_PAGE_SIZE) * HOST_PAGE_SIZE);
      setOpenHostId(null);
    }
  }, [offset, total]);

  const selectedSummary = hosts.find((host) => host.id === openHostId) ?? null;
  const selectedHostId = selectedSummary?.id ?? null;
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
  const selectedHost = detailQuery.data?.host ?? selectedSummary;
  const latest = detailQuery.data?.latest;
  const historyPoints = useMemo(
    () => [...(historyQuery.data?.points ?? [])]
      .sort((left, right) => left.collected_at.localeCompare(right.collected_at)),
    [historyQuery.data],
  );

  const closeDetailPanel = useCallback(() => {
    restoreDetailFocusRef.current = true;
    setOpenHostId(null);
  }, []);

  useEffect(() => {
    if (!selectedHost) return;
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") closeDetailPanel();
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [closeDetailPanel, selectedHost]);

  useLayoutEffect(() => {
    if (selectedHost || !restoreDetailFocusRef.current) return;
    restoreDetailFocusRef.current = false;
    const opener = detailPanelOpenerRef.current;
    detailPanelOpenerRef.current = null;
    if (opener?.isConnected && !opener.disabled) opener.focus();
  }, [selectedHost]);

  useLayoutEffect(() => {
    if (!selectedHost) return;
    const grid = hostGridRef.current;
    const panel = detailPanelRef.current;
    const selectedCard = grid?.querySelector<HTMLElement>('[data-detail-open="true"]');
    if (!grid || !panel || !selectedCard) return;

    const updatePosition = () => {
      const cards = Array.from(grid.querySelectorAll<HTMLElement>(".monitoring-host-card"));
      const selectedIndex = cards.indexOf(selectedCard);
      if (selectedIndex < 0) return;
      const gridStyle = window.getComputedStyle(grid);
      const columnCount = Math.max(
        1,
        gridStyle.gridTemplateColumns.split(/\s+/).filter(Boolean).length,
      );
      const cardRect = selectedCard.getBoundingClientRect();
      const gridRect = grid.getBoundingClientRect();
      const layout = adjacentPanelLayout({
        cardWidth: cardRect.width,
        cardHeight: cardRect.height,
        columnGap: Number.parseFloat(gridStyle.columnGap) || 0,
        rowGap: Number.parseFloat(gridStyle.rowGap) || 0,
        column: selectedIndex % columnCount,
        columnCount,
        top: cardRect.top - gridRect.top,
      });
      panel.style.left = `${layout.left}px`;
      panel.style.top = `${layout.top}px`;
      panel.style.width = `${layout.width}px`;
      panel.style.height = `${layout.height}px`;
      panel.style.borderRadius = `${cardRect.width / 18}px / ${cardRect.height / 12}px`;
      panel.dataset.placement = layout.placement;
      panel.style.visibility = "visible";
    };

    updatePosition();
    if (typeof ResizeObserver === "undefined") {
      window.addEventListener("resize", updatePosition);
      return () => window.removeEventListener("resize", updatePosition);
    }
    const resizeObserver = new ResizeObserver(updatePosition);
    resizeObserver.observe(grid);
    resizeObserver.observe(selectedCard);
    return () => resizeObserver.disconnect();
  }, [selectedHost]);

  const changePage = (nextOffset: number) => {
    setOpenHostId(null);
    setOffset(nextOffset);
  };

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
              className="sarmg-card__action"
              type="button"
              disabled={!hasPreviousPage}
              onClick={() => changePage(Math.max(0, offset - HOST_PAGE_SIZE))}
            >上一页</button>
            <span className="muted-inline">
              {offset + 1}–{Math.min(offset + hosts.length, total)} / {total}
            </span>
            <button
              className="sarmg-card__action"
              type="button"
              disabled={!hasNextPage}
              onClick={() => changePage(offset + HOST_PAGE_SIZE)}
            >下一页</button>
          </div>
        ) : null}
        <div className="monitoring-master-detail">
          <div className="sarmg-grid monitoring-host-grid" ref={hostGridRef}>
            {hosts.map((host) => (
              <HostRegistration
                key={host.id}
                host={host}
                selected={host.id === selectedHostId}
                onOpenDetails={(trigger) => {
                  if (openHostId === host.id) {
                    closeDetailPanel();
                    return;
                  }
                  detailPanelOpenerRef.current = trigger;
                  restoreDetailFocusRef.current = false;
                  setOpenHostId(host.id);
                }}
                onDeleted={() => {
                  if (openHostId === host.id) setOpenHostId(null);
                }}
              />
            ))}
          </div>
          {selectedHost ? (
            <aside
              ref={detailPanelRef}
              className="sunshine-adj-panel monitoring-adj-panel"
              role="dialog"
              aria-label={`${selectedHost.name} 详情面板`}
            >
              <MonitoringHostPanel
                key={selectedHost.id}
                host={selectedHost}
                report={latest}
                historyPoints={historyPoints}
                detailLoading={detailQuery.isLoading}
                detailError={detailQuery.error}
                historyLoading={historyQuery.isLoading}
                historyError={historyQuery.error}
                onClose={closeDetailPanel}
              />
            </aside>
          ) : null}
        </div>
      </section>
    </section>
  );
}
