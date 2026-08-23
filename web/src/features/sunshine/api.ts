import { request } from "../../shared/api/client";
import { pathSegment } from "../../shared/api/paths";
import type {
  SunshineApp,
  SunshineAppsResponse,
  SunshineClient,
  SunshineClientsResponse,
  SunshineConfig,
  SunshineHostInfo,
  SunshineHostPatchRequest,
  SunshineHostSaveRequest,
  SunshineLogsResponse,
} from "./types";

const sunshineHostPath = (id: string) => `/api/services/sunshine/hosts/${pathSegment(id)}`;
const MAX_COLLECTION_ITEMS = 512;
const MAX_OBJECT_KEYS = 256;
const MAX_DISPLAY_TEXT_CHARACTERS = 1_024;
const MAX_CLIENT_ID_CHARACTERS = 128;

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function isBoundedText(value: string, maxCharacters: number): boolean {
  let characters = 0;
  for (const character of value) {
    characters += 1;
    if (characters > maxCharacters) return false;
    const code = character.charCodeAt(0);
    if (code <= 0x1f || (code >= 0x7f && code <= 0x9f)) return false;
  }
  return true;
}

function hasSafeObjectShape(value: Record<string, unknown>): boolean {
  const keys = Object.keys(value);
  return keys.length <= MAX_OBJECT_KEYS && keys.every((key) => (
    isBoundedText(key, MAX_CLIENT_ID_CHARACTERS)
  ));
}

function isSafeDisplayText(value: unknown): value is string {
  return typeof value === "string"
    && isBoundedText(value, MAX_DISPLAY_TEXT_CHARACTERS);
}

function parseSunshineAppsResponse(value: unknown): SunshineAppsResponse {
  if (!isRecord(value) || !Array.isArray(value.apps) || value.apps.length > MAX_COLLECTION_ITEMS) {
    throw new Error("Sunshine 应用列表响应格式无效");
  }
  // Never filter malformed entries: the upstream array position is the mutation/delete id.
  const apps = value.apps.map((item, index): SunshineApp => {
    if (
      !isRecord(item)
      || !hasSafeObjectShape(item)
      || !isSafeDisplayText(item.name)
      || !(item.cmd === undefined || item.cmd === null || typeof item.cmd === "string")
    ) {
      throw new Error("Sunshine 应用列表响应格式无效");
    }
    return { ...item, name: item.name, cmd: item.cmd, index } as SunshineApp;
  });
  return { apps };
}

function parseSunshineClientsResponse(value: unknown): SunshineClientsResponse {
  if (
    !isRecord(value)
    || typeof value.status !== "boolean"
    || !Array.isArray(value.named_certs)
    || value.named_certs.length > MAX_COLLECTION_ITEMS
  ) {
    throw new Error("Sunshine 客户端列表响应格式无效");
  }
  const uuids = new Set<string>();
  const named_certs = value.named_certs.map((item): SunshineClient => {
    if (
      !isRecord(item)
      || !hasSafeObjectShape(item)
      || typeof item.uuid !== "string"
      || !isBoundedText(item.uuid, MAX_CLIENT_ID_CHARACTERS)
      || item.uuid.trim() !== item.uuid
      || !item.uuid
      || typeof item.enabled !== "boolean"
      || !(item.name === undefined || item.name === null || isSafeDisplayText(item.name))
      || uuids.has(item.uuid)
    ) {
      throw new Error("Sunshine 客户端列表响应格式无效");
    }
    uuids.add(item.uuid);
    return {
      ...item,
      name: item.name,
      uuid: item.uuid,
      enabled: item.enabled,
    };
  });
  return { status: value.status, named_certs };
}

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
  sunshineApps: async (id: string) => parseSunshineAppsResponse(
    await request<unknown>(`${sunshineHostPath(id)}/apps`),
  ),
  sunshineSaveApp: (id: string, app: Partial<SunshineApp>) => request<unknown>(
    `${sunshineHostPath(id)}/apps`,
    { method: "POST", body: JSON.stringify(app) },
  ),
  sunshineCloseApp: (id: string) => request<unknown>(`${sunshineHostPath(id)}/apps/close`, { method: "POST" }),
  sunshineDeleteApp: (id: string, index: number) => request<unknown>(
    `${sunshineHostPath(id)}/apps/${pathSegment(index)}`,
    { method: "DELETE" },
  ),
  sunshineClients: async (id: string) => parseSunshineClientsResponse(
    await request<unknown>(`${sunshineHostPath(id)}/clients`),
  ),
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
