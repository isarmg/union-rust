import { useCallback, useEffect, useState } from "react";
import { Copy, KeyRound, Plus, ShieldCheck, X } from "lucide-react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ActionButton,
  CardActions,
  InlineNotice,
  LoadingBlock,
  MutationError,
  SectionHeader,
  StatusLed,
} from "../../../shared/components/ui";
import { formatDateTime } from "../../../shared/lib/format";
import { monitoringApi as api } from "../api";
import { agentAuthorizationKeyGuidance, pendingAgentInstances } from "../model";
import { monitoringQueryKeys as queryKeys } from "../queryKeys";
import type { AgentInstanceSummary, CreatedAgentInstance, MonitoringHostSummary } from "../types";

const createAgentMutationKey = ["monitoring-create-agent-instance"] as const;

function removeMutationFromCache(queryClient: ReturnType<typeof useQueryClient>, mutationKey: readonly unknown[]) {
  const mutationCache = queryClient.getMutationCache();
  for (const mutation of mutationCache.findAll({ mutationKey, exact: true })) {
    mutationCache.remove(mutation);
  }
}

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
        <div><dt>显示名称</dt><dd>{created.display_name}</dd></div>
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
        <ActionButton icon={X} label="关闭并清除授权密钥" onClick={onClose} />
      </div>
    </div>
  );
}

/** 主机凭据管理：浏览器只接触一次性授权密钥，不再显示长期 Agent 令牌。 */
export function HostRegistration({ host }: { host: MonitoringHostSummary }) {
  const queryClient = useQueryClient();
  const [created, setCreated] = useState<CreatedAgentInstance | null>(null);
  const instancesQuery = useQuery({
    queryKey: queryKeys.monitoring.agentInstances,
    queryFn: api.monitoringAgentInstances,
    refetchInterval: 10_000,
  });
  const rePairMutation = useMutation({
    mutationKey: ["monitoring-re-pair-agent-instance", host.id],
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
    removeMutationFromCache(queryClient, ["monitoring-re-pair-agent-instance", host.id]);
  }, [host.id, queryClient, resetRePairMutation]);
  useEffect(() => () => {
    removeMutationFromCache(queryClient, ["monitoring-re-pair-agent-instance", host.id]);
  }, [host.id, queryClient]);
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
  const refreshedCreated = created
    ? instancesQuery.data?.find((instance) => instance.request_id === created.request_id)
    : undefined;
  const createdStatus = refreshedCreated?.status ?? created?.status;

  useEffect(() => {
    if (!created || !createdStatus || createdStatus === "pending") return;
    clearCreated();
  }, [clearCreated, created, createdStatus]);

  return (
    <section className="section-band">
      <SectionHeader
        icon={ShieldCheck}
        title="注册管理"
        description={host.lifecycle_status === "revoked"
          ? "该实例已撤销；可为同一实例重新配对，恢复后会继续使用原有历史。"
          : "重新配对只生成短时一次性授权密钥；撤销凭据会让 Agent 停止上报，同时保留主机和全部历史。"}
      />
      <CardActions>
        <ActionButton
          icon={KeyRound}
          label="重新配对"
          busy={rePairMutation.isPending}
          onClick={() => rePairMutation.mutate()}
        />
        {host.lifecycle_status === "active" ? (
          <ActionButton
            icon={X}
            label="撤销 Agent"
            tone="danger"
            busy={revokeMutation.isPending}
            onClick={() => window.confirm(
              `撤销 "${host.name}" 的 Agent 凭据？\n\n` +
                "Agent 将无法继续上报，但主机记录和历史数据会保留。之后可通过“重新配对”恢复。",
            ) && revokeMutation.mutate()}
          />
        ) : null}
      </CardActions>
      <MutationError mutation={rePairMutation} />
      <MutationError mutation={revokeMutation} />
      {created && createdStatus === "pending"
        ? <ActivationCodePanel created={created} onClose={clearCreated} />
        : null}
    </section>
  );
}

export function AgentInstances({ activeHostIds }: { activeHostIds: ReadonlySet<string> }) {
  const queryClient = useQueryClient();
  const [displayName, setDisplayName] = useState("新监控主机");
  const [expiresInMinutes, setExpiresInMinutes] = useState(15);
  const [created, setCreated] = useState<CreatedAgentInstance | null>(null);
  const [creationOutcome, setCreationOutcome] = useState<{
    displayName: string;
    status: AgentInstanceSummary["status"];
  } | null>(null);
  const instancesQuery = useQuery({
    queryKey: queryKeys.monitoring.agentInstances,
    queryFn: api.monitoringAgentInstances,
    refetchInterval: 10_000,
  });
  const createMutation = useMutation({
    mutationKey: createAgentMutationKey,
    mutationFn: () => api.monitoringCreateAgentInstance(displayName.trim(), expiresInMinutes),
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
    mutationFn: api.monitoringCancelAgentInstance,
    onSuccess: async (_result, requestId) => {
      if (created?.request_id === requestId) clearCreated();
      await queryClient.invalidateQueries({ queryKey: queryKeys.monitoring.agentInstances });
    },
  });
  const pending = pendingAgentInstances(instancesQuery.data ?? []);
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

  return (
    <section className="section-band agent-instances">
      <SectionHeader
        icon={Plus}
        title="创建 Agent"
        description="请先通过所属平台的软件分发渠道安装 UnionC Agent。Windows 可在 Agent 本地配置页填写 UnionC 服务器地址和管理台生成的一次性授权密钥；CLI 配对会打开激活页。管理中心不托管安装包，也不生成系统命令。"
      />
      <div className="agent-instance-form">
        <label>
          <span>显示名称</span>
          <input
            value={displayName}
            maxLength={128}
            onChange={(event) => setDisplayName(event.target.value)}
            placeholder="例如 办公室工作站"
          />
        </label>
        <label>
          <span>授权密钥有效期</span>
          <select value={expiresInMinutes} onChange={(event) => setExpiresInMinutes(Number(event.target.value))}>
            <option value={15}>15 分钟（推荐）</option>
            <option value={30}>30 分钟</option>
            <option value={60}>1 小时</option>
            <option value={1440}>24 小时</option>
          </select>
        </label>
        <ActionButton
          icon={Plus}
          label="创建 Agent"
          busy={createMutation.isPending}
          disabled={!displayName.trim()}
          onClick={() => createMutation.mutate()}
        />
      </div>
      <MutationError mutation={createMutation} />
      <MutationError mutation={cancelMutation} />
      {created && createdStatus === "pending"
        ? <ActivationCodePanel created={created} onClose={clearCreated} />
        : null}
      {creationOutcome?.status === "active" ? (
        <div className="agent-instance-activated" role="status">
          <ShieldCheck size={18} aria-hidden="true" />
          <span>{creationOutcome.displayName} 已激活，并已转入下方主机列表。</span>
          <ActionButton icon={X} label="关闭" onClick={() => setCreationOutcome(null)} />
        </div>
      ) : null}
      {creationOutcome && creationOutcome.status !== "active" ? (
        <InlineNotice tone="warn" text={`配对邀请已${creationOutcome.status === "expired" ? "过期" : "取消"}，授权密钥已从内存和页面清除。`} />
      ) : null}
      <div className="agent-pending-instances">
        <span className="muted-inline">待激活项</span>
        {instancesQuery.isLoading ? <LoadingBlock label="正在读取待激活项" /> : null}
        {instancesQuery.error ? <InlineNotice tone="danger" text={instancesQuery.error.message} /> : null}
        {!instancesQuery.isLoading && !instancesQuery.error && !pending.length
          ? <div className="empty-state">暂无待激活 Agent</div>
          : null}
        {pending.map((instance) => (
          <div className="agent-pending-instance" key={instance.request_id}>
            <div>
              <strong>{instance.display_name}</strong>
              <span className="muted-inline">待激活 · 到期 {formatDateTime(instance.expires_at)}</span>
              <span className="mono">请求 {instance.request_id}</span>
            </div>
            <div className="agent-pending-instance-actions">
              <ActionButton
                icon={X}
                label="取消"
                busy={cancelMutation.isPending && cancelMutation.variables === instance.request_id}
                onClick={() => window.confirm(
                  `取消 "${instance.display_name}" 的待激活邀请？\n\n此授权密钥将立即失效。`,
                ) && cancelMutation.mutate(instance.request_id)}
              />
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}
