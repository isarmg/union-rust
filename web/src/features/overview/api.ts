import { request } from "../../shared/api/client";
import type { ServiceStatus, SystemResources } from "./types";

export const overviewApi = {
  services: ({ signal }: { signal?: AbortSignal } = {}) =>
    request<ServiceStatus[]>("/api/services", { signal }),
  systemResources: () => request<SystemResources>("/api/system/resources"),
};
