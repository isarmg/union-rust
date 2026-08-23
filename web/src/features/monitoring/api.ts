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
const monitoringManagedInstancePath = (id: string) =>
  `/api/monitoring/managed-instances/${pathSegment(id)}`;
const monitoringAgentInstancePath = (requestId: string) =>
  `/api/monitoring/agent-instances/${pathSegment(requestId)}`;

export const monitoringApi = {
  monitoringHosts: (limit = 20, offset = 0) => request<MonitoringHostsResponse>(
    `/api/monitoring/hosts?limit=${pathSegment(limit)}&offset=${pathSegment(offset)}`,
  ),
  monitoringHost: (id: string) => request<MonitoringHostDetailResponse>(monitoringHostPath(id)),
  monitoringHistory: (id: string) => request<MonitoringHistoryResponse>(`${monitoringHostPath(id)}/history`),
  monitoringUpdateRemark: (id: string, remark: string) => request<void>(
    monitoringManagedInstancePath(id),
    {
      method: "PATCH",
      body: JSON.stringify({ remark }),
      expectedStatus: 204,
    },
  ),
  /** 永久删除主机、历史数据、凭据和关联邀请。 */
  monitoringDeleteHost: (id: string) => request<void>(
    monitoringManagedInstancePath(id),
    { method: "DELETE", expectedStatus: 204 },
  ),
  monitoringAgentInstances: (signal?: AbortSignal) => request<AgentInstanceSummary[]>(
    "/api/monitoring/agent-instances",
    { signal },
  ),
  monitoringCreateAgentInstance: (display_name: string, expires_in_minutes: number) =>
    request<CreatedAgentInstance>("/api/monitoring/agent-instances", {
      method: "POST",
      body: JSON.stringify({ display_name, expires_in_minutes }),
      expectedStatus: 201,
    }),
  monitoringCancelAgentInstance: (requestId: string) => request<void>(
    monitoringAgentInstancePath(requestId),
    { method: "DELETE", expectedStatus: 204 },
  ),
};
