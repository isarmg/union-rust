import { request } from "../../shared/api/client";
import type { ServiceStatus, SystemResources } from "./types";

export const overviewApi = {
  services: () => request<ServiceStatus[]>("/api/services"),
  systemResources: () => request<SystemResources>("/api/system/resources"),
};

