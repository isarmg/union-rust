import type {
  SunshineConfig,
  SunshineHostInfo,
  SunshineHostPatchRequest,
  SunshineHostSaveRequest,
  SunshineLogsResponse,
} from "./types";

const OPTIMISTIC_HOST_ID_PREFIX = "optimistic-sunshine-host:";
let optimisticHostSequence = 0;

export const sunshineHostMutationKeys = {
  create: ["sunshine-host-mutation", "create"] as const,
  update: ["sunshine-host-mutation", "update"] as const,
  delete: ["sunshine-host-mutation", "delete"] as const,
};

/** UnionC wraps Sunshine's current text/plain `/api/logs` body in `content`. */
export function sunshineLogLines(value: SunshineLogsResponse): string[] {
  return value.content.split(/\r?\n/);
}

export function parseSunshineConfigDraft(text: string): SunshineConfig {
  const value: unknown = JSON.parse(text);
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Sunshine 配置必须是 JSON 对象");
  }
  return value as SunshineConfig;
}

/**
 * Build a temporary host card while the create request is still running.
 *
 * Even a normal network/database round trip should not make the click look lost.
 * Keeping this entry in the React Query cache makes the user's action visible
 * immediately without pretending that connectivity is already known.
 */
export function optimisticSunshineHost(
  request: SunshineHostSaveRequest,
): SunshineHostInfo {
  optimisticHostSequence += 1;
  const host = request.host.trim();
  const urlHost = host.includes(":") && !host.startsWith("[") ? `[${host}]` : host;
  return {
    id: `${OPTIMISTIC_HOST_ID_PREFIX}${Date.now()}:${optimisticHostSequence}`,
    name: request.name.trim(),
    host,
    web_port: request.web_port,
    username: request.username.trim(),
    password_set: Boolean(request.password),
    verify_tls: request.verify_tls,
    web_url: `https://${urlHost}:${request.web_port}`,
    probe_status: "pending",
    reachable: null,
    connected: null,
    connection_error: "正在保存并检测连接…",
  };
}

export function isOptimisticSunshineHost(host: SunshineHostInfo): boolean {
  return host.id.startsWith(OPTIMISTIC_HOST_ID_PREFIX);
}

/** Only persisted hosts have valid server routes for apps/logs/config requests. */
export function persistedSunshineHosts(
  hosts: readonly SunshineHostInfo[],
): SunshineHostInfo[] {
  return hosts.filter((host) => !isOptimisticSunshineHost(host));
}

export interface SunshineHostUpdateOverlay {
  id: string;
  patch: SunshineHostPatchRequest;
  saved?: SunshineHostInfo;
}

/** Apply only fields present in PATCH while keeping response-only host state. */
export function applySunshineHostPatch(
  host: SunshineHostInfo,
  patch: SunshineHostPatchRequest,
): SunshineHostInfo {
  const next = { ...host };
  if (typeof patch.name === "string") next.name = patch.name;
  if (typeof patch.host === "string") next.host = patch.host;
  if (typeof patch.web_port === "number") next.web_port = patch.web_port;
  if (typeof patch.username === "string") next.username = patch.username;
  if (typeof patch.verify_tls === "boolean") next.verify_tls = patch.verify_tls;
  if (Object.hasOwn(patch, "password")) next.password_set = Boolean(patch.password);

  if (typeof patch.host === "string" || typeof patch.web_port === "number") {
    const urlHost = next.host.includes(":") && !next.host.startsWith("[")
      ? `[${next.host}]`
      : next.host;
    next.web_url = `https://${urlHost}:${next.web_port}`;
  }

  if (["host", "web_port", "username", "password", "verify_tls"].some((key) => Object.hasOwn(patch, key))) {
    next.probe_status = "pending";
    next.reachable = null;
    next.connected = null;
    next.connection_error = "正在保存并检测连接…";
  }
  return next;
}

/**
 * Reconcile a remote list without letting an out-of-order refresh overwrite
 * local mutations that have not received their response yet.
 */
export function mergeSunshineHostSnapshot(
  remote: readonly SunshineHostInfo[],
  current: readonly SunshineHostInfo[],
  deletingIds: ReadonlySet<string>,
  updateOverlays: readonly SunshineHostUpdateOverlay[] = [],
  createdHosts: readonly SunshineHostInfo[] = [],
): SunshineHostInfo[] {
  const next = remote.filter((host) => !deletingIds.has(host.id));
  const ids = new Set(next.map((host) => host.id));
  for (const host of current) {
    if (
      isOptimisticSunshineHost(host)
      && !deletingIds.has(host.id)
      && !ids.has(host.id)
    ) {
      next.push(host);
      ids.add(host.id);
    }
  }

  for (const created of createdHosts) {
    if (deletingIds.has(created.id)) continue;
    const index = next.findIndex((host) => host.id === created.id);
    if (index >= 0) next[index] = created;
    else {
      next.push(created);
      ids.add(created.id);
    }
  }
  for (const overlay of updateOverlays) {
    if (deletingIds.has(overlay.id)) continue;
    const index = next.findIndex((host) => host.id === overlay.id);
    const base = index >= 0
      ? next[index]
      : current.find((host) => host.id === overlay.id);
    if (!base) continue;
    const updated = overlay.saved ?? applySunshineHostPatch(base, overlay.patch);
    if (index >= 0) next[index] = updated;
    else {
      next.push(updated);
      ids.add(updated.id);
    }
  }
  return next;
}

/**
 * Do not let a list refetch overwrite an in-flight optimistic create. Once the
 * server has returned the real pending entry, poll briefly for its probe result.
 */
export function sunshineHostsRefetchInterval(
  hosts: readonly SunshineHostInfo[] | undefined,
  mutationCanBeOverwritten = false,
): number | false {
  if (mutationCanBeOverwritten || hosts?.some(isOptimisticSunshineHost)) return false;
  return hosts?.some((host) => host.probe_status === "pending") ? 1_500 : 30_000;
}

/** Replace a temporary/existing entry without disturbing concurrent mutations. */
export function replaceSunshineHost(
  hosts: readonly SunshineHostInfo[],
  host: SunshineHostInfo,
  previousId = host.id,
): SunshineHostInfo[] {
  const next = [...hosts];
  const previousIndex = next.findIndex((entry) => entry.id === previousId);
  const finalIndex = next.findIndex((entry) => entry.id === host.id);

  if (previousIndex >= 0) {
    next[previousIndex] = host;
    if (finalIndex >= 0 && finalIndex !== previousIndex) next.splice(finalIndex, 1);
    return next;
  }
  if (finalIndex >= 0) {
    next[finalIndex] = host;
    return next;
  }
  next.push(host);
  return next;
}

export function removeSunshineHost(
  hosts: readonly SunshineHostInfo[],
  id: string,
): SunshineHostInfo[] {
  return hosts.filter((host) => host.id !== id);
}

/** Reinsert only the failed deletion, preserving other concurrent cache edits. */
export function restoreSunshineHost(
  hosts: readonly SunshineHostInfo[],
  host: SunshineHostInfo,
  originalIndex: number,
): SunshineHostInfo[] {
  if (hosts.some((entry) => entry.id === host.id)) return [...hosts];
  const next = [...hosts];
  next.splice(Math.min(Math.max(originalIndex, 0), next.length), 0, host);
  return next;
}
