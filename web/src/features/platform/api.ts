import { request } from "../../shared/api/client";
import { pathSegment } from "../../shared/api/paths";
import type { JsonValue, ModuleConfiguration } from "./types";

export const platformApi = {
  // Runtime validation is intentionally owned by the Shell: one malformed plugin descriptor must
  // become an isolated diagnostic, not an unchecked cast that crashes all navigation.
  modules: () => request<unknown>("/api/platform/modules"),
  rescanModules: () => request<unknown>("/api/platform/modules/rescan", { method: "POST" }),
  enableModule: (moduleId: string) => request<unknown>(
    `/api/platform/modules/${pathSegment(moduleId)}/enable`,
    { method: "POST" },
  ),
  disableModule: (moduleId: string) => request<unknown>(
    `/api/platform/modules/${pathSegment(moduleId)}/disable`,
    { method: "POST" },
  ),
  moduleConfiguration: (moduleId: string) => request<ModuleConfiguration>(
    `/api/platform/modules/${pathSegment(moduleId)}/configuration`,
  ),
  saveModuleConfiguration: (moduleId: string, value: JsonValue) => request<ModuleConfiguration>(
    `/api/platform/modules/${pathSegment(moduleId)}/configuration`,
    { method: "PUT", body: JSON.stringify(value) },
  ),
};
