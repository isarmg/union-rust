import { FormEvent, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CheckCircle2, KeyRound, MonitorCog } from "lucide-react";
import { activationCodeForSubmission, canActivatePairing } from "./route";
import { agentActivationApi as api } from "./api";
import { InlineNotice, LoadingBlock, MutationError } from "../../shared/components/ui";
import { agentActivationQueryKeys as queryKeys } from "./queryKeys";
import { formatDateTime } from "../../shared/lib/format";
import { removeMutationFromCache } from "../../shared/lib/mutations";

function activationMutationKey(requestId: string) {
  return ["agent-activation", requestId] as const;
}

interface ActivationVariables {
  requestId: string;
  activationCode: string;
}

const pairingStatusLabel = {
  waiting: "等待激活",
  expired: "已过期",
  denied: "已拒绝",
  active: "已激活",
} as const;

export function AgentActivationPage({ requestId }: { requestId: string | null }) {
  const queryClient = useQueryClient();
  const [activationCode, setActivationCode] = useState("");
  const code = activationCodeForSubmission(activationCode);
  const pairingQuery = useQuery({
    queryKey: queryKeys.agentActivation.pairingRequest(requestId ?? ""),
    queryFn: () => api.agentPairingRequest(requestId!),
    enabled: Boolean(requestId),
    retry: false,
  });
  const activationMutation = useMutation({
    mutationKey: activationMutationKey(requestId ?? ""),
    mutationFn: ({ requestId: submittedRequestId, activationCode: submittedCode }: ActivationVariables) =>
      api.activateAgent(submittedRequestId, submittedCode),
    onSuccess: () => setActivationCode(""),
    onSettled: (_result, _error, variables) => {
      variables.activationCode = "";
      removeMutationFromCache(
        queryClient,
        activationMutationKey(variables.requestId),
        variables,
      );
    },
  });

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (requestId && code) activationMutation.mutate({ requestId, activationCode: code });
  };

  if (activationMutation.data) {
    return (
      <main className="app-shell sarmg-theme activation-screen" data-sarmg-scope data-sarmg-theme="system">
        <section className="activation-card" aria-labelledby="agent-activation-title">
          <div className="activation-heading success">
            <CheckCircle2 size={30} aria-hidden="true" />
            <div>
              <h1 id="agent-activation-title">Agent 激活成功</h1>
              <p>此设备已与 UnionC 配对，可以关闭这个浏览器窗口。</p>
            </div>
          </div>
          <dl className="activation-summary">
            <div>
              <dt>实例 ID</dt>
              <dd className="mono">{activationMutation.data.instance_id}</dd>
            </div>
            <div>
              <dt>状态</dt>
              <dd>已激活</dd>
            </div>
          </dl>
        </section>
      </main>
    );
  }

  return (
    <main className="app-shell sarmg-theme activation-screen" data-sarmg-scope data-sarmg-theme="system">
      <section className="activation-card" aria-labelledby="agent-activation-title">
        <div className="activation-heading">
          <MonitorCog size={30} aria-hidden="true" />
          <div>
            <h1 id="agent-activation-title">激活 UnionC Agent</h1>
            <p>CLI 配对会在此页面确认一次性授权密钥；Windows 可直接在 Agent 本地配置页填写服务器地址和授权密钥。</p>
          </div>
        </div>

        {requestId && pairingQuery.data ? (
          <dl className="activation-summary" aria-label="Agent 配对摘要">
            <div>
              <dt>系统</dt>
              <dd>{[pairingQuery.data.os, pairingQuery.data.arch].filter(Boolean).join(" · ") || "-"}</dd>
            </div>
            <div>
              <dt>Agent</dt>
              <dd>{pairingQuery.data.agent_version || "-"}</dd>
            </div>
            <div>
              <dt>状态</dt>
              <dd>{pairingStatusLabel[pairingQuery.data.status]}</dd>
            </div>
            <div>
              <dt>到期时间</dt>
              <dd>{formatDateTime(pairingQuery.data.expires_at)}</dd>
            </div>
            <div>
              <dt>配对请求</dt>
              <dd className="mono">{requestId}</dd>
            </div>
          </dl>
        ) : null}
        {!requestId ? (
          <div className="activation-route-error" role="alert">
            激活链接无效或不完整。请返回 Agent 程序重新发起浏览器配对。
          </div>
        ) : null}

        {requestId && pairingQuery.isLoading ? <LoadingBlock label="正在读取 Agent 配对信息" /> : null}
        {requestId && pairingQuery.error ? (
          <InlineNotice tone="danger" text={pairingQuery.error.message} />
        ) : null}
        {requestId && pairingQuery.data && !canActivatePairing(pairingQuery.data.status) ? (
          <InlineNotice
            tone={pairingQuery.data.status === "active" ? "warn" : "danger"}
            text={`此配对请求${pairingStatusLabel[pairingQuery.data.status]}，不能再次激活。`}
          />
        ) : null}

        {requestId && pairingQuery.data && canActivatePairing(pairingQuery.data.status) ? (
          <form className="activation-form" onSubmit={submit}>
            <label htmlFor="agent-activation-code">
              <span><KeyRound size={16} aria-hidden="true" />一次性激活码</span>
              <input
                id="agent-activation-code"
                value={activationCode}
                onChange={(event) => {
                  setActivationCode(event.target.value);
                  if (activationMutation.isError) activationMutation.reset();
                }}
                autoComplete="one-time-code"
                autoCapitalize="none"
                spellCheck={false}
                maxLength={128}
                placeholder="输入管理中心生成的激活码"
                autoFocus
                required
              />
            </label>
            <MutationError mutation={activationMutation} />
            <button
              className="action-button primary activation-submit"
              type="submit"
              disabled={!code || activationMutation.isPending}
            >
              <KeyRound size={16} aria-hidden="true" />
              <span>{activationMutation.isPending ? "正在激活…" : "确认激活"}</span>
            </button>
          </form>
        ) : null}
      </section>
    </main>
  );
}
