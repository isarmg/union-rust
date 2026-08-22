import { useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Terminal } from "lucide-react";
import { api } from "../api";
import { queryKeys } from "../query-keys";
import { querySunshineHosts } from "../sunshine-host-query";
import type { LogsResponse } from "../types";
import { InlineNotice, LoadingBlock, LogViewer, SectionHeader } from "../components/ui";
import {
  persistedSunshineHosts,
  sunshineHostsRefetchInterval,
  sunshineLogLines,
} from "../sunshine-data";

export const MAX_RENDERED_LOG_LINES = 2_000;

export function limitLogLines(lines: readonly string[], limit = MAX_RENDERED_LOG_LINES): string[] {
  if (lines.length <= limit) return [...lines];
  return [`… 已省略前 ${lines.length - limit} 行，仅显示最新 ${limit} 行`, ...lines.slice(-limit)];
}

export function LogsView() {
  const queryClient = useQueryClient();
  const [preferredHostId, setPreferredHostId] = useState<string | null>(null);
  const hostsQuery = useQuery({
    queryKey: queryKeys.sunshine.hosts,
    queryFn: ({ signal }) => querySunshineHosts(queryClient, signal),
    refetchInterval: (query) => sunshineHostsRefetchInterval(query.state.data),
  });
  const hosts = persistedSunshineHosts(hostsQuery.data ?? []);
  const selectedHostId = preferredHostId && hosts.some((host) => host.id === preferredHostId)
    ? preferredHostId
    : (hosts[0]?.id ?? null);

  const logsQuery = useQuery({
    queryKey: queryKeys.logs.sunshine(selectedHostId ?? ""),
    queryFn: () => api.sunshineApiLogs(selectedHostId!),
    enabled: Boolean(selectedHostId),
    refetchInterval: 30_000,
  });
  const selectedHost = hosts.find((host) => host.id === selectedHostId);
  const logLines = useMemo(
    () => logsQuery.data === undefined ? undefined : limitLogLines(sunshineLogLines(logsQuery.data)),
    [logsQuery.data],
  );
  const logs: LogsResponse | undefined = logLines && selectedHost
    ? { path: `Sunshine API · ${selectedHost.name}`, lines: logLines }
    : undefined;

  return (
    <section className="view-stack logs-view-stack">
      <section className="section-band">
        <SectionHeader icon={Terminal} title="日志" description="按需读取单台 Sunshine 主机，页面最多保留最新 2,000 行。" />
        {hostsQuery.isLoading ? <LoadingBlock label="读取主机列表" /> : null}
        {hostsQuery.error ? <InlineNotice tone="danger" text={`主机列表读取失败：${hostsQuery.error.message}`} /> : null}
        {!hostsQuery.isLoading && !hostsQuery.error && !hosts.length
          ? <InlineNotice tone="warn" text="暂无已配置的 Sunshine 主机" />
          : null}
        {hosts.length ? (
          <label className="logs-host-selector">
            <span>主机</span>
            <select value={selectedHostId ?? ""} onChange={(event) => setPreferredHostId(event.target.value)}>
              {hosts.map((host) => <option key={host.id} value={host.id}>{host.name}</option>)}
            </select>
          </label>
        ) : null}
        {logsQuery.error ? <InlineNotice tone="danger" text={`日志读取失败：${logsQuery.error.message}`} /> : null}
        {selectedHost ? <LogViewer logs={logs} loading={logsQuery.isLoading} /> : null}
      </section>
    </section>
  );
}
