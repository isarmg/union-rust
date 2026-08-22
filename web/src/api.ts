import type {
  MonitoringHistoryResponse, MonitoringHostDetailResponse, MonitoringHostsResponse, ServiceStatus,
  AgentActivationResponse, AgentInstanceSummary, AgentPairingRequestSummary, CreatedAgentInstance, SunshineApp, SunshineAppsResponse,
  SunshineClientsResponse, SunshineConfig, SunshineLogsResponse,
  SunshineHostInfo, SunshineHostPatchRequest, SunshineHostSaveRequest, SystemResources,
} from "./types";
import {
  agentPairingRequestPath,
  monitoringAgentInstancePath,
  monitoringHostPath,
  pathSegment,
  sunshineHostPath,
} from "./api-paths";

const REQUEST_TIMEOUT_MS = 15_000;
type ApiRequestInit = RequestInit & {
  timeoutMs?: number;
  suppressAuthExpired?: boolean;
  expectedStatus?: number;
};

export class ApiError extends Error {
  constructor(message: string, readonly code: string | undefined, readonly status: number) {
    super(message); this.name = "ApiError";
  }
}

async function readApiError(response: Response): Promise<ApiError> {
  const text = await response.text().catch(() => "");
  const fallback = text || `${response.status} ${response.statusText}`;
  try {
    const payload = JSON.parse(text) as Record<string, unknown>;
    const code = typeof payload.code === "string" ? payload.code : undefined;
    const message = payload.message;
    return new ApiError(typeof message === "string" ? message : fallback, code, response.status);
  } catch { return new ApiError(fallback, undefined, response.status); }
}

/**
 * 读取登录时下发的 CSRF 令牌。
 *
 * 会话 cookie 是 HttpOnly（JS 读不到），CSRF cookie 刻意不是——双提交模式要求
 * 前端把它回填到请求头，服务端再与该会话存储的令牌比对。跨站页面读不到本站
 * cookie，因此攻击者无法伪造这个头。
 *
 * 生产环境使用 `__Host-` 前缀（要求 Secure），开发环境为普通名字。
 */
function csrfToken(): string {
  for (const name of ["__Host-csrf", "csrf"]) {
    const match = document.cookie.match(new RegExp(`(?:^|;\\s*)${name}=([^;]*)`));
    if (match) return decodeURIComponent(match[1]);
  }
  return "";
}

async function request<T>(path: string, init?: ApiRequestInit): Promise<T> {
  const {
    timeoutMs = REQUEST_TIMEOUT_MS,
    suppressAuthExpired = false,
    expectedStatus = 200,
    ...fetchInit
  } = init ?? {};
  const controller = new AbortController();
  let didTimeout = false;
  const timeoutId = timeoutMs > 0 ? window.setTimeout(() => { didTimeout = true; controller.abort(); }, timeoutMs) : undefined;
  const callerSignal = fetchInit.signal;
  const abortFromCaller = () => controller.abort(callerSignal?.reason);
  if (callerSignal?.aborted) abortFromCaller();
  else callerSignal?.addEventListener("abort", abortFromCaller, { once: true });
  let response: Response;
  try {
    const shouldSendJson = Boolean(fetchInit.body) && !(fetchInit.body instanceof FormData);
    response = await fetch(path, {
      ...fetchInit, credentials: "include", signal: controller.signal,
      headers: {
        ...(shouldSendJson ? { "Content-Type": "application/json" } : undefined),
        ...(!fetchInit.method || ["GET", "HEAD", "OPTIONS"].includes(fetchInit.method.toUpperCase()) ? undefined : { "X-CSRF-Token": csrfToken() }),
        ...fetchInit.headers,
      },
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError") {
      throw new Error(didTimeout ? "请求超时，请检查 UnionC 是否可用" : "请求已取消", {
        cause: error,
      });
    }
    throw error;
  } finally {
    if (timeoutId !== undefined) window.clearTimeout(timeoutId);
    callerSignal?.removeEventListener("abort", abortFromCaller);
  }
  if (response.status === 401) {
    if (suppressAuthExpired) throw await readApiError(response);
    window.dispatchEvent(new Event("unionc:auth-expired"));
    throw new ApiError("认证已失效，请重新登录", "unauthorized", 401);
  }
  if (!response.ok) throw await readApiError(response);
  if (response.status !== expectedStatus) {
    throw new ApiError(
      `UnionC 返回了非当前契约状态：应为 ${expectedStatus}，实际为 ${response.status}`,
      "unexpected_status",
      response.status,
    );
  }
  if (expectedStatus === 204) return undefined as T;
  const mediaType = response.headers.get("Content-Type")?.split(";", 1)[0]?.trim().toLowerCase();
  if (mediaType !== "application/json") {
    throw new ApiError(
      "UnionC 返回了非当前契约媒体类型，应为 application/json",
      "unexpected_content_type",
      response.status,
    );
  }
  return await response.json() as T;
}

export const api = {
  authenticate: () => request<{ username: string }>("/api/auth/me", { suppressAuthExpired: true }),
  login: async (username: string, password: string) => {
    try {
      return await request<{ username: string }>("/api/auth/login", { method: "POST", body: JSON.stringify({ username, password }), suppressAuthExpired: true });
    } catch (error) {
      if (error instanceof ApiError && error.status === 401) throw new ApiError("账号或密码错误", error.code, 401);
      throw error;
    }
  },
  logout: () => request<void>("/api/auth/logout", { method: "POST", expectedStatus: 204 }),
  changePassword: (current_password: string, new_password: string) => request<void>("/api/auth/change-password", { method: "POST", body: JSON.stringify({ current_password, new_password }), expectedStatus: 204 }),
  services: () => request<ServiceStatus[]>("/api/services"),
  systemResources: () => request<SystemResources>("/api/system/resources"),
  issueSseTicket: () => request<{ ticket: string }>("/api/events/ticket", { method: "POST" }),
  monitoringHosts: (limit = 20, offset = 0) =>
    request<MonitoringHostsResponse>(`/api/monitoring/hosts?limit=${pathSegment(limit)}&offset=${pathSegment(offset)}`),
  monitoringHost: (id: string) => request<MonitoringHostDetailResponse>(monitoringHostPath(id)),
  monitoringHistory: (id: string) => request<MonitoringHistoryResponse>(`${monitoringHostPath(id)}/history`),
  /** 只吊销 Agent 凭据，保留主机和历史数据。 */
  monitoringRevokeHost: (id: string) => request<void>(`${monitoringHostPath(id)}/revoke`, { method: "POST", expectedStatus: 204 }),
  monitoringAgentInstances: () =>
    request<AgentInstanceSummary[]>("/api/monitoring/agent-instances"),
  monitoringCreateAgentInstance: (display_name: string, expires_in_minutes: number, instance_id?: string) =>
    request<CreatedAgentInstance>("/api/monitoring/agent-instances", {
      method: "POST",
      body: JSON.stringify({ display_name, expires_in_minutes, ...(instance_id ? { instance_id } : {}) }),
      expectedStatus: 201,
    }),
  monitoringCancelAgentInstance: (requestId: string) =>
    request<void>(monitoringAgentInstancePath(requestId), { method: "DELETE", expectedStatus: 204 }),
  activateAgent: async (request_id: string, activation_code: string) => {
    try {
      return await request<AgentActivationResponse>("/api/agent/v2/activate", {
        method: "POST",
        body: JSON.stringify({ request_id, activation_code }),
        suppressAuthExpired: true,
      });
    } catch (error) {
      if (error instanceof ApiError && error.status === 401) {
        throw new ApiError("激活码无效或已过期", error.code, error.status);
      }
      throw error;
    }
  },
  agentPairingRequest: (requestId: string) =>
    request<AgentPairingRequestSummary>(agentPairingRequestPath(requestId), {
      suppressAuthExpired: true,
    }),

  sunshineHosts: (signal?: AbortSignal) => request<SunshineHostInfo[]>("/api/services/sunshine/hosts", { signal }),
  sunshineCreateHost: (body: SunshineHostSaveRequest) => request<SunshineHostInfo>("/api/services/sunshine/hosts", { method: "POST", body: JSON.stringify(body), expectedStatus: 201 }),
  sunshineUpdateHost: (id: string, body: SunshineHostPatchRequest) => request<SunshineHostInfo>(sunshineHostPath(id), { method: "PATCH", body: JSON.stringify(body) }),
  sunshineDeleteHost: (id: string) => request<void>(sunshineHostPath(id), { method: "DELETE", expectedStatus: 204 }),
  sunshineApiLogs: (id: string) => request<SunshineLogsResponse>(`${sunshineHostPath(id)}/api-logs`),
  sunshineApps: (id: string) => request<SunshineAppsResponse>(`${sunshineHostPath(id)}/apps`),
  sunshineSaveApp: (id: string, app: Partial<SunshineApp>) => request<unknown>(`${sunshineHostPath(id)}/apps`, { method: "POST", body: JSON.stringify(app) }),
  sunshineCloseApp: (id: string) => request<unknown>(`${sunshineHostPath(id)}/apps/close`, { method: "POST" }),
  sunshineDeleteApp: (id: string, index: number) => request<unknown>(`${sunshineHostPath(id)}/apps/${pathSegment(index)}`, { method: "DELETE" }),
  sunshineClients: (id: string) => request<SunshineClientsResponse>(`${sunshineHostPath(id)}/clients`),
  sunshineUnpairClient: (id: string, uuid: string) => request<unknown>(`${sunshineHostPath(id)}/clients/unpair`, { method: "POST", body: JSON.stringify({ uuid }) }),
  sunshineUnpairAll: (id: string) => request<unknown>(`${sunshineHostPath(id)}/clients/unpair-all`, { method: "POST" }),
  sunshineUpdateClient: (id: string, uuid: string, enabled: boolean) => request<unknown>(`${sunshineHostPath(id)}/clients/update`, { method: "POST", body: JSON.stringify({ uuid, enabled }) }),
  sunshineConfig: (id: string) => request<SunshineConfig>(`${sunshineHostPath(id)}/config`),
  sunshineSaveConfig: (id: string, config: SunshineConfig) => request<unknown>(`${sunshineHostPath(id)}/config`, { method: "POST", body: JSON.stringify(config) }),
  sunshinePin: (id: string, pin: string, name: string) => request<unknown>(`${sunshineHostPath(id)}/pin`, { method: "POST", body: JSON.stringify({ pin, name }) }),
  sunshineRestart: (id: string) => request<unknown>(`${sunshineHostPath(id)}/restart`, { method: "POST" }),
  sunshineResetDisplay: (id: string) => request<unknown>(`${sunshineHostPath(id)}/reset-display`, { method: "POST" }),
};
