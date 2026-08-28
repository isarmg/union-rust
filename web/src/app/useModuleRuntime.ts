import { useEffect, useMemo, useState } from "react";
import { parseModuleCatalog, type CatalogIssue, type PlatformModule } from "./moduleCatalog";
import { moduleRuntimeEnvironment, type FailedWebModule, type LoadedWebModule } from "./moduleRuntime";

export interface ModuleRuntimeSnapshot {
  catalog: PlatformModule[];
  catalogIssues: CatalogIssue[];
  loaded: LoadedWebModule[];
  failed: FailedWebModule[];
  loading: boolean;
}

const EMPTY_SNAPSHOT: ModuleRuntimeSnapshot = {
  catalog: [],
  catalogIssues: [],
  loaded: [],
  failed: [],
  loading: false,
};

export function canLoadModuleFrontend(
  module: PlatformModule,
  permissions: readonly string[],
): boolean {
  if (!module.enabled || module.frontend === null) return false;
  const granted = new Set(permissions);
  const allowed = (permission: string) => granted.has("*") || granted.has(permission);
  return module.frontend.routes.some((route) => allowed(route.permission));
}

export function useModuleRuntime(
  catalogPayload: unknown,
  permissions: readonly string[],
): ModuleRuntimeSnapshot {
  const parsed = useMemo(() => (
    catalogPayload === undefined ? { modules: [], issues: [] } : parseModuleCatalog(catalogPayload)
  ), [catalogPayload]);
  // Health timestamps and process ids change during normal polling and must not reload executable
  // frontend code. A module package/version or its validated Web contract is immutable.
  const fingerprint = JSON.stringify(parsed.modules.map((module) => ({
    id: module.id,
    version: module.version,
    enabled: module.enabled,
    compatibility: module.compatibility,
    frontend: module.frontend,
    resolved_frontend: module.resolved_frontend,
  })));
  const permissionFingerprint = JSON.stringify([...permissions].sort());
  const [snapshot, setSnapshot] = useState<ModuleRuntimeSnapshot>(EMPTY_SNAPSHOT);

  useEffect(() => {
    let cancelled = false;
    const owned: LoadedWebModule[] = [];
    const candidates = parsed.modules.filter((module) => canLoadModuleFrontend(module, permissions));
    setSnapshot({
      catalog: parsed.modules,
      catalogIssues: parsed.issues,
      loaded: [],
      failed: [],
      loading: candidates.length > 0,
    });
    let pending = candidates.length;
    candidates.forEach((module) => { void (async () => {
      let result: { loaded?: LoadedWebModule; failed?: FailedWebModule };
      try {
        result = { loaded: await moduleRuntimeEnvironment.load(module) };
      } catch (error) {
        result = {
          failed: {
            manifest: module,
            error: error instanceof Error ? error : new Error("模块加载失败"),
          },
        };
      }
      if (cancelled) {
        result.loaded?.dispose();
        return;
      }
      pending -= 1;
      if (result.loaded) owned.push(result.loaded);
      setSnapshot((current) => ({
        ...current,
        loaded: result.loaded ? [...current.loaded, result.loaded] : current.loaded,
        failed: result.failed ? [...current.failed, result.failed] : current.failed,
        loading: pending > 0,
      }));
    })(); });
    return () => {
      cancelled = true;
      owned.splice(0).forEach((module) => module.dispose());
    };
  // The serialized, validated catalog avoids unloading plugins merely because React Query returned
  // a new array instance with identical Manifest values.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [fingerprint, permissionFingerprint]);

  return snapshot;
}
