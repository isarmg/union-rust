import { request } from "../../shared/api/client";
import type { ServiceStatus } from "./types";

export const overviewApi = {
  services: ({ signal }: { signal?: AbortSignal } = {}) =>
    request<ServiceStatus[]>("/api/services", { signal }),
};
