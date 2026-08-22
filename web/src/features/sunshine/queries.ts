import type { QueryClient } from "@tanstack/react-query";
import { sunshineApi as api } from "./api";
import { sunshineQueryKeys as queryKeys } from "./queryKeys";
import {
  mergeSunshineHostSnapshot,
  sunshineHostMutationKeys,
} from "./data";
import type { SunshineHostInfo, SunshineHostPatchRequest } from "./types";

interface SunshineHostUpdateVariables {
  id: string;
  patch: SunshineHostPatchRequest;
}

function updateVariables(value: unknown): SunshineHostUpdateVariables | null {
  if (!value || typeof value !== "object") return null;
  const candidate = value as Partial<SunshineHostUpdateVariables>;
  if (typeof candidate.id !== "string" || !candidate.patch || typeof candidate.patch !== "object") return null;
  return { id: candidate.id, patch: candidate.patch };
}

function savedHost(value: unknown, id: string): SunshineHostInfo | undefined {
  if (!value || typeof value !== "object") return undefined;
  const candidate = value as Partial<SunshineHostInfo>;
  return candidate.id === id ? value as SunshineHostInfo : undefined;
}

/**
 * Fetch the authoritative snapshot while preserving mutations that are still in
 * flight. This also protects explicit global invalidation, not only timers.
 */
export async function querySunshineHosts(
  queryClient: QueryClient,
  signal?: AbortSignal,
): Promise<SunshineHostInfo[]> {
  const mutationCache = queryClient.getMutationCache();
  const createMutationsAtStart = new Map(
    mutationCache.findAll({
      mutationKey: sunshineHostMutationKeys.create,
      exact: true,
    }).map((mutation) => [mutation, mutation.state.status] as const),
  );
  const updateMutationsAtStart = new Map(
    mutationCache.findAll({
      mutationKey: sunshineHostMutationKeys.update,
      exact: true,
    }).map((mutation) => [mutation, mutation.state.status] as const),
  );
  const deleteMutationsAtStart = new Map(
    mutationCache.findAll({
      mutationKey: sunshineHostMutationKeys.delete,
      exact: true,
    }).map((mutation) => [mutation, mutation.state.status] as const),
  );
  const remote = await api.sunshineHosts(signal);
  const current = queryClient.getQueryData<SunshineHostInfo[]>(
    queryKeys.sunshine.hosts,
  ) ?? [];
  const deletingIds = new Set(mutationCache.findAll({
      mutationKey: sunshineHostMutationKeys.delete,
      exact: true,
    }).flatMap((mutation) => {
      const id = mutation.state.variables;
      if (typeof id !== "string" || mutation.state.status === "error") return [];
      return deleteMutationsAtStart.get(mutation) === "success" ? [] : [id];
    }));
  const updateOverlays = mutationCache.findAll({
    mutationKey: sunshineHostMutationKeys.update,
    exact: true,
  }).flatMap((mutation) => {
    const variables = updateVariables(mutation.state.variables);
    if (!variables || mutation.state.status === "error") return [];
    // A GET that began after this PATCH completed is authoritative. Older GETs
    // must retain either the pending patch or the PATCH response snapshot.
    if (updateMutationsAtStart.get(mutation) === "success") return [];
    return [{
      ...variables,
      saved: mutation.state.status === "success"
        ? savedHost(mutation.state.data, variables.id)
        : undefined,
    }];
  });
  const createdHosts = mutationCache.findAll({
    mutationKey: sunshineHostMutationKeys.create,
    exact: true,
  }).flatMap((mutation) => {
    if (
      mutation.state.status !== "success"
      || createMutationsAtStart.get(mutation) === "success"
    ) return [];
    const result = mutation.state.data;
    if (!result || typeof result !== "object" || typeof (result as Partial<SunshineHostInfo>).id !== "string") return [];
    return [result as SunshineHostInfo];
  });
  return mergeSunshineHostSnapshot(
    remote,
    current,
    deletingIds,
    updateOverlays,
    createdHosts,
  );
}
