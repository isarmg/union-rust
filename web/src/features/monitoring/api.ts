import { request } from "../../shared/api/client";
import { pathSegment } from "../../shared/api/paths";
import type {
  AgentInstanceSummary,
  CreatedAgentInstance,
  MonitoringHistoryResponse,
  MonitoringHostDetailResponse,
  MonitoringHostsResponse,
} from "./types";

const monitoringHostPath = (id: string) => `/api/monitoring/hosts/${pathSegment(id)}`;
const monitoringAgentInstancePath = (requestId: string) =>
  `/api/monitoring/agent-instances/${pathSegment(requestId)}`;

export const monitoringApi = {
  monitoringHosts: (limit = 20, offset = 0) => request<MonitoringHostsResponse>(
    `/api/monitoring/hosts?limit=${pathSegment(limit)}&offset=${pathSegment(offset)}`,
  ),
  monitoringHost: (id: string) => request<MonitoringHostDetailResponse>(monitoringHostPath(id)),
  monitoringHistory: (id: string) => request<MonitoringHistoryResponse>(`${monitoringHostPath(id)}/history`),
  /** 只吊销 Agent 凭据，保留主机和历史数据。 */
  monitoringRevokeHost: (id: string) => request<void>(
    `${monitoringHostPath(id)}/revoke`,
    { method: "POST", expectedStatus: 204 },
  ),
  monitoringAgentInstances: () => request<AgentInstanceSummary[]>("/api/monitoring/agent-instances"),
  monitoringCreateAgentInstance: (display_name: string, expires_in_minutes: number, instance_id?: string) =>
    request<CreatedAgentInstance>("/api/monitoring/agent-instances", {
      method: "POST",
      body: JSON.stringify({ display_name, expires_in_minutes, ...(instance_id ? { instance_id } : {}) }),
      expectedStatus: 201,
    }),
  monitoringCancelAgentInstance: (requestId: string) => request<void>(
    monitoringAgentInstancePath(requestId),
    { method: "DELETE", expectedStatus: 204 },
  ),
};
