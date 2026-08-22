import { ToggleLeft, ToggleRight, Unlink, Users } from "lucide-react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ActionButton,
  InlineNotice,
  LoadingBlock,
  MutationError,
  SectionHeader,
  StatusLed,
} from "../../../shared/components/ui";
import { sunshineApi as api } from "../api";
import { sunshineQueryKeys as queryKeys } from "../queryKeys";
import type { SunshineClient, SunshineClientsResponse, SunshineHostInfo } from "../types";

function extractClients(data: SunshineClientsResponse | undefined): SunshineClient[] {
  return data ? data.named_certs : [];
}

export function ClientsSection({ host }: { host: SunshineHostInfo }) {
  const queryClient = useQueryClient();
  const queryKey = queryKeys.sunshine.clients(host.id);
  const query = useQuery({ queryKey, queryFn: () => api.sunshineClients(host.id) });
  const unpairMutation = useMutation({
    mutationFn: (uuid: string) => api.sunshineUnpairClient(host.id, uuid),
    onSuccess: async () => queryClient.invalidateQueries({ queryKey }),
  });
  const unpairAllMutation = useMutation({
    mutationFn: () => api.sunshineUnpairAll(host.id),
    onSuccess: async () => queryClient.invalidateQueries({ queryKey }),
  });
  const updateMutation = useMutation({
    mutationFn: ({ uuid, enabled }: { uuid: string; enabled: boolean }) =>
      api.sunshineUpdateClient(host.id, uuid, enabled),
    onSuccess: async () => queryClient.invalidateQueries({ queryKey }),
  });
  const clients = extractClients(query.data);

  return (
    <section className="section-band">
      <SectionHeader
        icon={Users}
        title="客户端"
        actions={(
          <ActionButton
            icon={Unlink}
            label="取消所有配对"
            tone="danger"
            busy={unpairAllMutation.isPending}
            onClick={() => window.confirm("取消所有配对？") && unpairAllMutation.mutate()}
          />
        )}
      />
      <MutationError mutation={unpairMutation} />
      <MutationError mutation={unpairAllMutation} />
      <MutationError mutation={updateMutation} />
      {query.isLoading ? <LoadingBlock label="读取客户端" /> : null}
      {query.error ? <InlineNotice tone="danger" text={query.error.message} /> : null}
      <div className="sunshine-client-list">
        {clients.map((client) => (
          <div className="sunshine-client-item" key={client.uuid}>
            <div className="sunshine-client-info">
              <strong>{client.name ?? "未命名设备"}</strong>
              <span className="mono">{client.uuid}</span>
              <span className="sunshine-client-status">
                <StatusLed tone={client.enabled ? "good" : "warn"} />
                {client.enabled ? "已启用" : "已禁用"}
              </span>
            </div>
            <div className="button-row">
              <button
                className="icon-button"
                type="button"
                title={client.enabled ? "禁用" : "启用"}
                aria-label={`${client.enabled ? "禁用" : "启用"}客户端 ${client.name ?? client.uuid}`}
                disabled={updateMutation.isPending}
                onClick={() => updateMutation.mutate({ uuid: client.uuid, enabled: !client.enabled })}
              >
                {client.enabled ? <ToggleRight size={18} /> : <ToggleLeft size={18} />}
              </button>
              <button
                className="icon-button danger"
                type="button"
                title="取消配对"
                disabled={unpairMutation.isPending}
                aria-label={`取消客户端 ${client.name ?? client.uuid} 的配对`}
                onClick={() => window.confirm(`取消设备 "${client.name ?? client.uuid}" 的配对？`) && unpairMutation.mutate(client.uuid)}
              >
                <Unlink size={15} />
              </button>
            </div>
          </div>
        ))}
        {!query.isLoading && !clients.length ? <p className="muted-inline">暂无已配对客户端。</p> : null}
      </div>
    </section>
  );
}
