import { useEffect, useRef, useState } from "react";
import { Boxes } from "lucide-react";
import { useMutation, useMutationState, useQuery, useQueryClient } from "@tanstack/react-query";
import { ContentTitle, InlineNotice, LoadingBlock, MutationError } from "../../shared/components/ui";
import { sunshineApi as api } from "./api";
import {
  applySunshineHostPatch,
  isOptimisticSunshineHost,
  optimisticSunshineHost,
  removeSunshineHost,
  replaceSunshineHost,
  restoreSunshineHost,
  sunshineHostMutationKeys,
  sunshineHostsRefetchInterval,
} from "./data";
import { querySunshineHosts } from "./queries";
import { sunshineQueryKeys as queryKeys } from "./queryKeys";
import type { SunshineHostInfo, SunshineHostPatchRequest, SunshineHostSaveRequest } from "./types";
import { HostCard } from "./components/HostCard";
import { HostPanel } from "./components/HostPanel";

// Keep the tested feature helpers available from the public view module.
export { InlineHostField } from "./components/HostCard";
export { appDraft } from "./components/AppsSection";

export function SunshineView({ addTrigger = 0 }: { addTrigger?: number }) {
  const queryClient = useQueryClient();
  const createInFlightRef = useRef(false);
  const deletingHostIdsRef = useRef(new Set<string>());
  const hostsQuery = useQuery({
    queryKey: queryKeys.sunshine.hosts,
    queryFn: ({ signal }) => querySunshineHosts(queryClient, signal),
    refetchInterval: (query) => sunshineHostsRefetchInterval(
      query.state.data,
      deletingHostIdsRef.current.size > 0,
    ),
  });
  const hosts = hostsQuery.data ?? [];
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const handledAddTriggerRef = useRef(0);
  const panelOpen = selectedId !== null;

  const createMutation = useMutation({
    mutationKey: sunshineHostMutationKeys.create,
    mutationFn: (request: SunshineHostSaveRequest) => api.sunshineCreateHost(request),
    onMutate: async (request) => {
      await queryClient.cancelQueries({ queryKey: queryKeys.sunshine.hosts, exact: true });
      const optimistic = optimisticSunshineHost(request);
      queryClient.setQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts, (current) => [
        ...(current ?? []),
        optimistic,
      ]);
      return { optimisticId: optimistic.id };
    },
    onSuccess: (saved, _request, context) => {
      queryClient.setQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts, (current) =>
        replaceSunshineHost(current ?? [], saved, context.optimisticId));
    },
    onError: (_error, _request, context) => {
      if (!context) return;
      queryClient.setQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts, (current) =>
        removeSunshineHost(current ?? [], context.optimisticId));
    },
    onSettled: () => {
      createInFlightRef.current = false;
      void queryClient.invalidateQueries({ queryKey: queryKeys.sunshine.hosts, exact: true });
    },
  });

  const updateMutation = useMutation({
    mutationKey: sunshineHostMutationKeys.update,
    mutationFn: ({ id, patch }: { id: string; patch: SunshineHostPatchRequest }) =>
      api.sunshineUpdateHost(id, patch),
    onMutate: async ({ id, patch }) => {
      await queryClient.cancelQueries({ queryKey: queryKeys.sunshine.hosts, exact: true });
      const previous = queryClient.getQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts)
        ?.find((host) => host.id === id);
      if (previous) {
        queryClient.setQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts, (current) =>
          replaceSunshineHost(current ?? [], applySunshineHostPatch(previous, patch)));
      }
      return { previous };
    },
    onSuccess: (saved) => {
      queryClient.setQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts, (current) =>
        replaceSunshineHost(current ?? [], saved));
    },
    onError: (_error, { id }, context) => {
      if (!context?.previous) return;
      queryClient.setQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts, (current) =>
        replaceSunshineHost(current ?? [], context.previous!, id));
    },
    onSettled: (_result, _error, variables) => {
      if (Object.hasOwn(variables.patch, "password")) delete variables.patch.password;
      void queryClient.invalidateQueries({ queryKey: queryKeys.sunshine.hosts, exact: true });
    },
  });
  const pendingUpdates = useMutationState<{ id: string; patch: SunshineHostPatchRequest }>({
    filters: {
      mutationKey: sunshineHostMutationKeys.update,
      exact: true,
      status: "pending",
    },
    select: (mutation) => mutation.state.variables as { id: string; patch: SunshineHostPatchRequest },
  });
  const updatingHostIds = new Set(pendingUpdates.map(({ id }) => id));

  const deleteMutation = useMutation({
    mutationKey: sunshineHostMutationKeys.delete,
    mutationFn: (id: string) => api.sunshineDeleteHost(id),
    onMutate: async (id) => {
      await queryClient.cancelQueries({ queryKey: queryKeys.sunshine.hosts, exact: true });
      const current = queryClient.getQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts) ?? [];
      const originalIndex = current.findIndex((host) => host.id === id);
      const removed = originalIndex >= 0 ? current[originalIndex] : undefined;
      queryClient.setQueryData<SunshineHostInfo[]>(
        queryKeys.sunshine.hosts,
        removeSunshineHost(current, id),
      );
      if (selectedId === id) setSelectedId(null);
      return { originalIndex, removed };
    },
    onSuccess: (_result, id) => {
      queryClient.setQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts, (current) =>
        removeSunshineHost(current ?? [], id));
      queryClient.removeQueries({ queryKey: queryKeys.sunshine.apps(id), exact: true });
      queryClient.removeQueries({ queryKey: queryKeys.sunshine.clients(id), exact: true });
      queryClient.removeQueries({ queryKey: queryKeys.sunshine.config(id), exact: true });
      queryClient.removeQueries({ queryKey: queryKeys.logs.sunshine(id), exact: true });
    },
    onError: (_error, _id, context) => {
      if (!context?.removed) return;
      queryClient.setQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts, (current) =>
        restoreSunshineHost(current ?? [], context.removed!, context.originalIndex));
    },
    onSettled: (_result, _error, id) => {
      deletingHostIdsRef.current.delete(id);
      void queryClient.invalidateQueries({ queryKey: queryKeys.sunshine.hosts, exact: true });
    },
  });

  const selectedHost = hosts.find((host) => host.id === selectedId) ?? null;

  function createDefaultHost() {
    if (createInFlightRef.current) return;
    const usedNames = new Set(hosts.map((host) => host.name));
    let index = hosts.length + 1;
    while (usedNames.has(`Sunshine ${index}`)) index += 1;
    createInFlightRef.current = true;
    createMutation.mutate({
      name: `Sunshine ${index}`,
      host: "192.168.1.2",
      web_port: 47990,
      username: "admin",
      password: null,
      verify_tls: true,
    });
    setSelectedId(null);
  }

  useEffect(() => {
    if (addTrigger <= handledAddTriggerRef.current) return;
    handledAddTriggerRef.current = addTrigger;
    createDefaultHost();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [addTrigger]);

  function deleteHost(id: string) {
    if (deletingHostIdsRef.current.has(id)) return;
    deletingHostIdsRef.current.add(id);
    deleteMutation.mutate(id);
  }

  return (
    <section className="view-stack">
      <section className="section-band sunshine-new-section">
        <MutationError mutation={createMutation} />
        <MutationError mutation={updateMutation} />
        <MutationError mutation={deleteMutation} />
        {hostsQuery.error ? <InlineNotice tone="danger" text={hostsQuery.error.message} /> : null}
        {hostsQuery.isLoading ? <LoadingBlock label="读取主机" /> : null}
        <div className="instance-list-title"><ContentTitle icon={Boxes} title="实例" /></div>
        <div className={`sunshine-master-detail${panelOpen ? " has-panel" : ""}`}>
          <div className="content-grid sunshine-host-grid">
            {hosts.map((host) => (
              <HostCard
                key={host.id}
                host={host}
                selected={selectedId === host.id}
                updating={updatingHostIds.has(host.id)}
                onOpen={() => {
                  if (isOptimisticSunshineHost(host)) return;
                  setSelectedId((current) => current === host.id ? null : host.id);
                }}
                onInlineUpdate={(patch) => updateMutation.mutateAsync({ id: host.id, patch }).then(() => undefined)}
                onDelete={() => {
                  if (isOptimisticSunshineHost(host)) return;
                  if (window.confirm(`确定删除主机 "${host.name}"？`)) deleteHost(host.id);
                }}
              />
            ))}
          </div>
          {selectedHost ? (
            <aside className="sunshine-adj-panel" aria-label={`${selectedHost.name} 管理面板`}>
              <HostPanel key={selectedHost.id} host={selectedHost} />
            </aside>
          ) : null}
        </div>
      </section>
    </section>
  );
}
