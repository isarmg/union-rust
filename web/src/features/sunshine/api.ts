import { request } from "../../shared/api/client";
import { pathSegment } from "../../shared/api/paths";
import type {
  SunshineApp,
  SunshineAppsResponse,
  SunshineClientsResponse,
  SunshineConfig,
  SunshineHostInfo,
  SunshineHostPatchRequest,
  SunshineHostSaveRequest,
  SunshineLogsResponse,
} from "./types";

const sunshineHostPath = (id: string) => `/api/services/sunshine/hosts/${pathSegment(id)}`;

export const sunshineApi = {
  sunshineHosts: (signal?: AbortSignal) => request<SunshineHostInfo[]>("/api/services/sunshine/hosts", { signal }),
  sunshineCreateHost: (body: SunshineHostSaveRequest) => request<SunshineHostInfo>(
    "/api/services/sunshine/hosts",
    { method: "POST", body: JSON.stringify(body), expectedStatus: 201 },
  ),
  sunshineUpdateHost: (id: string, body: SunshineHostPatchRequest) => request<SunshineHostInfo>(
    sunshineHostPath(id),
    { method: "PATCH", body: JSON.stringify(body) },
  ),
  sunshineDeleteHost: (id: string) => request<void>(sunshineHostPath(id), { method: "DELETE", expectedStatus: 204 }),
  sunshineApiLogs: (id: string) => request<SunshineLogsResponse>(`${sunshineHostPath(id)}/api-logs`),
  sunshineApps: (id: string) => request<SunshineAppsResponse>(`${sunshineHostPath(id)}/apps`),
  sunshineSaveApp: (id: string, app: Partial<SunshineApp>) => request<unknown>(
    `${sunshineHostPath(id)}/apps`,
    { method: "POST", body: JSON.stringify(app) },
  ),
  sunshineCloseApp: (id: string) => request<unknown>(`${sunshineHostPath(id)}/apps/close`, { method: "POST" }),
  sunshineDeleteApp: (id: string, index: number) => request<unknown>(
    `${sunshineHostPath(id)}/apps/${pathSegment(index)}`,
    { method: "DELETE" },
  ),
  sunshineClients: (id: string) => request<SunshineClientsResponse>(`${sunshineHostPath(id)}/clients`),
  sunshineUnpairClient: (id: string, uuid: string) => request<unknown>(
    `${sunshineHostPath(id)}/clients/unpair`,
    { method: "POST", body: JSON.stringify({ uuid }) },
  ),
  sunshineUnpairAll: (id: string) => request<unknown>(`${sunshineHostPath(id)}/clients/unpair-all`, { method: "POST" }),
  sunshineUpdateClient: (id: string, uuid: string, enabled: boolean) => request<unknown>(
    `${sunshineHostPath(id)}/clients/update`,
    { method: "POST", body: JSON.stringify({ uuid, enabled }) },
  ),
  sunshineConfig: (id: string) => request<SunshineConfig>(`${sunshineHostPath(id)}/config`),
  sunshineSaveConfig: (id: string, config: SunshineConfig) => request<unknown>(
    `${sunshineHostPath(id)}/config`,
    { method: "POST", body: JSON.stringify(config) },
  ),
  sunshinePin: (id: string, pin: string, name: string) => request<unknown>(
    `${sunshineHostPath(id)}/pin`,
    { method: "POST", body: JSON.stringify({ pin, name }) },
  ),
  sunshineRestart: (id: string) => request<unknown>(`${sunshineHostPath(id)}/restart`, { method: "POST" }),
  sunshineResetDisplay: (id: string) => request<unknown>(`${sunshineHostPath(id)}/reset-display`, { method: "POST" }),
};
