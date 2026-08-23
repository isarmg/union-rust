import type {
  AgentInstanceSummary,
  MonitoringHistoryPoint,
  MonitoringHostSummary,
} from "./types";

export const NA = "N/A";

export function isNumber(value: number | null | undefined): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

export function formatMetric(
  value: number | null | undefined,
  formatter: (value: number) => string,
): string {
  return isNumber(value) ? formatter(value) : NA;
}

export function formatPercent(value: number | null | undefined): string {
  return formatMetric(value, (metric) => `${metric.toFixed(1)}%`);
}

export function formatTemperature(value: number | null | undefined): string {
  return formatMetric(value, (metric) => `${metric.toFixed(1)} °C`);
}

export function sumNullable(...values: Array<number | null | undefined>): number | null {
  const available = values.filter(isNumber);
  return available.length ? available.reduce((total, value) => total + value, 0) : null;
}

export function metricTone(
  value: number | null | undefined,
  threshold = 85,
): "good" | "warn" | "neutral" {
  if (!isNumber(value)) return "neutral";
  return value >= threshold ? "warn" : "good";
}

export function statusMeta(status: MonitoringHostSummary["status"]) {
  if (status === "online") return { label: "在线", tone: "good" as const };
  if (status === "stale") return { label: "数据过期", tone: "warn" as const };
  return { label: "离线", tone: "danger" as const };
}

export function historyValues(
  points: MonitoringHistoryPoint[],
  read: (point: MonitoringHistoryPoint) => number | null,
): Array<number | null> {
  return points.map((point) => {
    const value = read(point);
    return isNumber(value) ? value : null;
  });
}

export function latestHistoryValue(values: Array<number | null>): number | null {
  return values.length ? values[values.length - 1] : null;
}

export function pendingAgentInstances(instances: AgentInstanceSummary[]): AgentInstanceSummary[] {
  return instances.filter((instance) => instance.status === "pending");
}

export const agentAuthorizationKeyGuidance =
  "授权密钥只在本次创建后显示。Windows 请在 Agent 托盘的“本地配置”页填写服务器地址和此密钥；CLI 配对请在 Agent 打开的激活页确认。";
