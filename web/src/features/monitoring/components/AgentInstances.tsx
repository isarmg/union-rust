import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Copy, Eye, KeyRound, Loader2, ShieldCheck, Trash2, X } from "lucide-react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { InlineEditableField } from "../../../shared/components/InlineEditableField";
import {
  ActionButton,
  CardActions,
  CardInner,
  CardRow,
  InlineNotice,
  MutationError,
  StatusLed,
  TruncatedText,
} from "../../../shared/components/ui";
import { formatDateTime } from "../../../shared/lib/format";
import { removeMutationFromCache } from "../../../shared/lib/mutations";
import { monitoringApi as api } from "../api";
import { agentAuthorizationKeyGuidance, statusMeta } from "../model";
import { monitoringQueryKeys as queryKeys } from "../queryKeys";
import type { AgentInstanceSummary, CreatedAgentInstance, MonitoringHostSummary } from "../types";

const createAgentMutationKey = ["monitoring-create-agent-instance"] as const;

function ActivationCodePanel({ created, onClose }: {
  created: CreatedAgentInstance;
  onClose: () => void;
}) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="agent-created-instance">
      <InlineNotice tone="warn" text={agentAuthorizationKeyGuidance} />
      <dl className="agent-instance-details">
        <div><dt>状态</dt><dd><StatusLed tone="warn" /> 待激活</dd></div>
        <div><dt>Server 备注</dt><dd>{created.display_name}</dd></div>
        <div><dt>一次性授权密钥</dt><dd className="agent-activation-code">{created.activation_code}</dd></div>
        {created.instance_id ? <div><dt>实例 ID</dt><dd className="mono">{created.instance_id}</dd></div> : null}
        <div><dt>到期时间</dt><dd>{formatDateTime(created.expires_at)}</dd></div>
      </dl>
      <div className="button-row">
        <ActionButton
          icon={Copy}
          label={copied ? "已复制授权密钥" : "复制授权密钥"}
          onClick={() => {
            void navigator.clipboard.writeText(created.activation_code)
              .then(() => setCopied(true))
              .catch(() => setCopied(false));
          }}
        />
        <ActionButton icon={X} label="取消邀请并清除授权密钥" onClick={onClose} />
      </div>
    </div>
  );
}

/** 主机内容块同时承载选择、名称编辑和完整的 Agent 生命周期管理。 */
export function HostRegistration({
  host,
  selected = false,
  onSelect = () => undefined,
  onDeleted,
}: {
  host: MonitoringHostSummary;
  selected?: boolean;
  onSelect?: () => void;
  onDeleted?: () => void;
}) {
  const queryClient = useQueryClient();
  const [created, setCreated] = useState<CreatedAgentInstance | null>(null);
  const instancesQuery = useQuery({
    queryKey: queryKeys.monitoring.agentInstances,
    queryFn: ({ signal }) => api.monitoringAgentInstances(signal),
    refetchInterval: 10_000,
  });
  const rePairMutationKey = useMemo(
    () => ["monitoring-re-pair-agent-instance", host.id] as const,
    [host.id],
  );
  const rePairMutation = useMutation({
    mutationKey: rePairMutationKey,
    mutationFn: () => api.monitoringCreateAgentInstance(host.name, 15, host.id),
    onSuccess: async (result) => {
      setCreated(result);
      await queryClient.invalidateQueries({ queryKey: queryKeys.monitoring.agentInstances });
    },
  });
  const resetRePairMutation = rePairMutation.reset;
  const clearCreated = useCallback(() => {
    setCreated(null);
    resetRePairMutation();
    removeMutationFromCache(queryClient, rePairMutationKey);
  }, [queryClient, rePairMutationKey, resetRePairMutation]);
  useEffect(() => () => {
    removeMutationFromCache(queryClient, rePairMutationKey);
  }, [queryClient, rePairMutationKey]);

  const cancelMutation = useMutation({
    mutationFn: (requestId: string) => api.monitoringCancelAgentInstance(requestId),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: queryKeys.monitoring.agentInstances });
    },
  });
  const remarkMutation = useMutation({
    mutationFn: (remark: string) => api.monitoringUpdateRemark(host.id, remark),
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: queryKeys.monitoring.hosts }),
        queryClient.invalidateQueries({ queryKey: queryKeys.monitoring.host(host.id) }),
      ]);
    },
  });
  const revokeMutation = useMutation({
    mutationFn: () => api.monitoringRevokeHost(host.id),
    onSuccess: async () => {
      clearCreated();
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: queryKeys.monitoring.hosts }),
        queryClient.invalidateQueries({ queryKey: queryKeys.monitoring.host(host.id) }),
      ]);
    },
  });
  const deleteMutation = useMutation({
    mutationFn: () => api.monitoringDeleteHost(host.id),
    onSuccess: async () => {
      clearCreated();
      queryClient.removeQueries({ queryKey: queryKeys.monitoring.host(host.id), exact: true });
      queryClient.removeQueries({ queryKey: queryKeys.monitoring.history(host.id), exact: true });
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: queryKeys.monitoring.hosts }),
        queryClient.invalidateQueries({ queryKey: queryKeys.monitoring.agentInstances }),
      ]);
      onDeleted?.();
    },
  });
  const refreshedCreated = created
    ? instancesQuery.data?.find((instance) => instance.request_id === created.request_id)
    : undefined;
  const createdStatus = refreshedCreated?.status ?? created?.status;
  const pendingInvite = instancesQuery.data?.find((instance) => (
    instance.instance_id === host.id && instance.status === "pending"
  ));

  useEffect(() => {
    if (!created || !createdStatus || createdStatus === "pending") return;
    clearCreated();
  }, [clearCreated, created, createdStatus]);

  const cancelCreated = () => {
    const requestId = created?.request_id;
    clearCreated();
    if (requestId) cancelMutation.mutate(requestId);
  };
  const status = statusMeta(host.status);
  const controlsBusy = remarkMutation.isPending
    || rePairMutation.isPending
    || revokeMutation.isPending
    || deleteMutation.isPending
    || cancelMutation.isPending;

  return (
    <div className="monitoring-host-entry">
      <article
        className={`content-card monitoring-host-card${selected ? " selected" : ""}`}
        aria-label={`${host.name}，${status.label}`}
        aria-busy={controlsBusy}
      >
        <CardInner>
          <CardRow label="Server 备注">
            <InlineEditableField
              label="备注"
              value={host.name}
              validate={(value) => value && value.length <= 255 ? null : "Server 备注必须为 1–255 个字符"}
              onSave={(remark) => remarkMutation.mutateAsync(remark).then(() => undefined)}
              maxLength={255}
              disabled={controlsBusy}
            />
            <span title={status.label}><StatusLed tone={status.tone} /></span>
          </CardRow>
          <CardRow label="状态">{status.label}</CardRow>
          <CardRow label="系统">
            <TruncatedText>{[host.os, host.arch].filter(Boolean).join(" · ")}</TruncatedText>
          </CardRow>
          <CardActions>
            <button className="card-action-button" type="button" disabled={controlsBusy} onClick={onSelect}>
              <Eye size={12} /><span>详情</span>
            </button>
            <button
              className="card-action-button"
              type="button"
              disabled={controlsBusy || Boolean(pendingInvite)}
              title={pendingInvite ? "该主机已有待激活的重新配对邀请" : undefined}
              onClick={() => rePairMutation.mutate()}
            >
              {rePairMutation.isPending ? <Loader2 className="spin" size={12} /> : <KeyRound size={12} />}
              <span>重新配对</span>
            </button>
            {host.lifecycle_status === "active" ? (
              <button
                className="card-action-button danger"
                type="button"
                disabled={controlsBusy}
                onClick={() => window.confirm(
                  `撤销 "${host.name}" 的 Agent 凭据？\n\n` +
                    "Agent 将无法继续上报，但主机记录和历史数据会保留。之后可通过“重新配对”恢复。",
                ) && revokeMutation.mutate()}
              >
                <X size={12} /><span>撤销</span>
              </button>
            ) : null}
            <button
              className="card-action-button danger"
              type="button"
              disabled={controlsBusy}
              onClick={() => window.confirm(
                `永久删除主机 "${host.name}"？\n\n` +
                  "此操作会删除该实例的全部历史、凭据和邀请，无法撤销。",
              ) && deleteMutation.mutate()}
            >
              <Trash2 size={12} /><span>删除</span>
            </button>
          </CardActions>
        </CardInner>
      </article>
      <MutationError mutation={remarkMutation} />
      <MutationError mutation={rePairMutation} />
      <MutationError mutation={revokeMutation} />
      <MutationError mutation={deleteMutation} />
      <MutationError mutation={cancelMutation} />
      {created && createdStatus === "pending"
        ? <ActivationCodePanel created={created} onClose={cancelCreated} />
        : null}
      {pendingInvite && pendingInvite.request_id !== created?.request_id ? (
        <div className="monitoring-host-invite" role="status">
          <InlineNotice
            tone="warn"
            text={`重新配对邀请待激活，到期时间 ${formatDateTime(pendingInvite.expires_at)}。一次性授权密钥不会再次显示。`}
          />
          <ActionButton
            icon={X}
            label="取消待激活邀请"
            busy={cancelMutation.isPending && cancelMutation.variables === pendingInvite.request_id}
            onClick={() => window.confirm(
              `取消 "${host.name}" 的待激活邀请？\n\n此前生成的一次性授权密钥将立即失效。`,
            ) && cancelMutation.mutate(pendingInvite.request_id)}
          />
        </div>
      ) : null}
    </div>
  );
}

/** 侧栏“+”触发默认 15 分钟邀请；空闲时不占用主机页面空间。 */
export function AgentInstances({
  activeHostIds,
  addTrigger = 0,
}: {
  activeHostIds: ReadonlySet<string>;
  addTrigger?: number;
}) {
  const queryClient = useQueryClient();
  const handledAddTriggerRef = useRef(0);
  const [created, setCreated] = useState<CreatedAgentInstance | null>(null);
  const [creationOutcome, setCreationOutcome] = useState<{
    displayName: string;
    status: AgentInstanceSummary["status"];
  } | null>(null);
  const instancesQuery = useQuery({
    queryKey: queryKeys.monitoring.agentInstances,
    queryFn: ({ signal }) => api.monitoringAgentInstances(signal),
    refetchInterval: 10_000,
    enabled: Boolean(created),
  });
  const createMutation = useMutation({
    mutationKey: createAgentMutationKey,
    mutationFn: () => api.monitoringCreateAgentInstance("新监控主机", 15),
    onSuccess: async (result) => {
      setCreationOutcome(null);
      setCreated(result);
      await queryClient.invalidateQueries({ queryKey: queryKeys.monitoring.agentInstances });
    },
  });
  const resetCreateMutation = createMutation.reset;
  const clearCreated = useCallback(() => {
    setCreated(null);
    resetCreateMutation();
    removeMutationFromCache(queryClient, createAgentMutationKey);
  }, [queryClient, resetCreateMutation]);
  useEffect(() => () => {
    removeMutationFromCache(queryClient, createAgentMutationKey);
  }, [queryClient]);
  const cancelMutation = useMutation({
    mutationFn: (requestId: string) => api.monitoringCancelAgentInstance(requestId),
  });
  const createAgent = createMutation.mutate;
  const creationPending = createMutation.isPending;

  useEffect(() => {
    if (addTrigger <= handledAddTriggerRef.current) return;
    handledAddTriggerRef.current = addTrigger;
    if (created || creationPending) return;
    createAgent();
  }, [addTrigger, createAgent, created, creationPending]);

  const refreshedCreated = created
    ? instancesQuery.data?.find((instance) => instance.request_id === created.request_id)
    : undefined;
  const createdStatus = created?.instance_id && activeHostIds.has(created.instance_id)
    ? "active"
    : (refreshedCreated?.status ?? created?.status);

  useEffect(() => {
    if (!created || !createdStatus || createdStatus === "pending") return;
    setCreationOutcome({ displayName: created.display_name, status: createdStatus });
    clearCreated();
  }, [clearCreated, created, createdStatus]);

  const cancelCreated = () => {
    const requestId = created?.request_id;
    clearCreated();
    if (requestId) cancelMutation.mutate(requestId);
  };
  const visible = createMutation.isPending
    || createMutation.isError
    || cancelMutation.isError
    || Boolean(created)
    || Boolean(creationOutcome);
  if (!visible) return null;

  return (
    <section className="section-band agent-instances" aria-live="polite">
      {createMutation.isPending ? <InlineNotice tone="warn" text="正在创建 Agent 邀请…" /> : null}
      <MutationError mutation={createMutation} />
      <MutationError mutation={cancelMutation} />
      {created && createdStatus === "pending"
        ? <ActivationCodePanel created={created} onClose={cancelCreated} />
        : null}
      {creationOutcome?.status === "active" ? (
        <div className="agent-instance-activated" role="status">
          <ShieldCheck size={18} aria-hidden="true" />
          <span>{creationOutcome.displayName} 已激活，并已加入主机列表。</span>
          <ActionButton icon={X} label="关闭" onClick={() => setCreationOutcome(null)} />
        </div>
      ) : null}
      {creationOutcome && creationOutcome.status !== "active" ? (
        <InlineNotice tone="warn" text={`配对邀请已${creationOutcome.status === "expired" ? "过期" : "取消"}，授权密钥已从内存和页面清除。`} />
      ) : null}
    </section>
  );
}
