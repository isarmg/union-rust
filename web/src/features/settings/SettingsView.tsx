import { useEffect, useMemo, useState } from "react";
import { Boxes, KeyRound, Loader2, Play, RefreshCw, Save, Square } from "lucide-react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { authApi as api } from "../auth/api";
import { authQueryKeys as queryKeys } from "../auth/queryKeys";
import { CardActions, CardInner, CardRow, InlineNotice, SectionHeader, TruncatedText } from "../../shared/components/ui";
import { removeMutationFromCache } from "../../shared/lib/mutations";
import { parseModuleCatalog, type ModuleHealthState, type PlatformModule } from "../../app/moduleCatalog";
import { platformApi } from "../platform/api";
import { platformQueryKeys } from "../platform/queryKeys";
import type { JsonValue, ModuleConfiguration } from "../platform/types";

const changePasswordMutationKey = ["settings-change-password"] as const;
const moduleConfigurationMutationKey = (moduleId: string) => [
  "settings-module-configuration",
  moduleId,
] as const;

interface ChangePasswordVariables {
  currentPassword: string;
  newPassword: string;
}

// ─── 账号管理区域 ─────────────────────────────────────────────────────────────

function AccountSection({ onPasswordChanged }: { onPasswordChanged: () => void }) {
  const queryClient = useQueryClient();
  const meQuery = useQuery({ queryKey: queryKeys.me, queryFn: api.authenticate });
  const [currentPw, setCurrentPw] = useState("");
  const [newPw, setNewPw] = useState("");
  const [confirmPw, setConfirmPw] = useState("");

  const changeMutation = useMutation({
    mutationKey: changePasswordMutationKey,
    mutationFn: ({ currentPassword, newPassword }: ChangePasswordVariables) =>
      api.changePassword(currentPassword, newPassword),
    onSuccess: () => {
      setCurrentPw("");
      setNewPw("");
      setConfirmPw("");
      // The server revoked this session in the same password transaction. Do not issue a second
      // logout request: a delayed mutation callback could otherwise send it with a newer cookie.
      onPasswordChanged();
    },
    onSettled: (_result, _error, variables) => {
      variables.currentPassword = "";
      variables.newPassword = "";
      removeMutationFromCache(queryClient, changePasswordMutationKey, variables);
    },
  });

  const passwordMismatch = confirmPw.length > 0 && newPw !== confirmPw;
  const canSubmit =
    currentPw.length > 0 && newPw.length >= 12 && newPw === confirmPw && !changeMutation.isPending;
  const feedback = changeMutation.isError
    ? { tone: "danger", text: changeMutation.error.message }
    : passwordMismatch
      ? { tone: "warn", text: "两次输入的新密码不一致" }
      : changeMutation.isPending
        ? { tone: "muted", text: "正在修改密码…" }
        : { tone: "muted", text: "新密码至少 12 个字符" };

  return (
    <section className="section-band">
      <SectionHeader icon={KeyRound} title="账号管理" />
      {meQuery.error ? <InlineNotice tone="danger" text={`账号信息读取失败：${meQuery.error.message}`} /> : null}
      <div className="sarmg-grid settings-grid">
        <form
          className="sarmg-card setting-item account-card-form"
          aria-label="修改管理员密码"
          onSubmit={(event) => {
            event.preventDefault();
            if (!canSubmit) return;
            changeMutation.mutate({ currentPassword: currentPw, newPassword: newPw });
          }}
        >
          <CardInner>
            <CardRow label="用户">
              <TruncatedText>{meQuery.data?.username ?? "—"}</TruncatedText>
            </CardRow>
            <CardRow label="原密码">
              <input
                className="account-card-input"
                type="password"
                value={currentPw}
                onChange={(event) => setCurrentPw(event.target.value)}
                aria-label="原密码"
                autoComplete="current-password"
                placeholder="输入原密码"
              />
            </CardRow>
            <CardRow label="新密码">
              <input
                className="account-card-input"
                type="password"
                value={newPw}
                onChange={(event) => setNewPw(event.target.value)}
                aria-label="新密码"
                autoComplete="new-password"
                placeholder="至少 12 个字符"
              />
            </CardRow>
            <CardRow label="确认新密码">
              <input
                className={`account-card-input${passwordMismatch ? " input-error" : ""}`}
                type="password"
                value={confirmPw}
                onChange={(event) => setConfirmPw(event.target.value)}
                aria-label="确认新密码"
                aria-invalid={passwordMismatch}
                autoComplete="new-password"
                placeholder="再次输入新密码"
              />
            </CardRow>
            <CardRow label="提示">
              <TruncatedText
                className={`account-card-feedback ${feedback.tone}`}
                role={feedback.tone === "danger" ? "alert" : "status"}
                aria-live={feedback.tone === "danger" ? "assertive" : "polite"}
                title={feedback.text}
              >
                {feedback.text}
              </TruncatedText>
            </CardRow>
            <CardActions>
              <button
                type="submit"
                className="sarmg-card__action sarmg-action-primary"
                disabled={!canSubmit}
              >
                {changeMutation.isPending
                  ? <Loader2 size={12} className="spin" aria-hidden="true" />
                  : <Save size={12} aria-hidden="true" />}
                <span>{changeMutation.isPending ? "正在修改…" : "修改密码"}</span>
              </button>
            </CardActions>
          </CardInner>
        </form>
      </div>
    </section>
  );
}

type ParsedDraft = { value: JsonValue; error: null } | { value: null; error: string };

function isJsonValue(value: unknown): value is JsonValue {
  if (value === null || typeof value === "string" || typeof value === "boolean") return true;
  if (typeof value === "number") return Number.isFinite(value);
  if (Array.isArray(value)) return value.every(isJsonValue);
  if (typeof value !== "object") return false;
  return Object.values(value).every(isJsonValue);
}

function parseConfigurationDraft(draft: string): ParsedDraft {
  try {
    const value: unknown = JSON.parse(draft);
    if (!isJsonValue(value)) return { value: null, error: "配置包含 JSON 不支持的值" };
    if (value === null || Array.isArray(value) || typeof value !== "object") {
      return { value: null, error: "配置根节点必须是 JSON 对象" };
    }
    return { value, error: null };
  } catch {
    return { value: null, error: "JSON 格式无效" };
  }
}

function containsRedactedSecret(value: JsonValue | null): boolean {
  if (value === "***") return true;
  if (Array.isArray(value)) return value.some(containsRedactedSecret);
  if (value !== null && typeof value === "object") {
    return Object.values(value).some(containsRedactedSecret);
  }
  return false;
}

function lifecycleLabel(state: ModuleHealthState): string {
  const labels: Record<ModuleHealthState, string> = {
    discovered: "已发现",
    installing: "正在准备",
    starting: "正在启动",
    available: "运行正常",
    degraded: "运行降级",
    backoff: "等待重启",
    incompatible: "版本不兼容",
    stopped: "已停止",
    failed: "启动失败",
  };
  return labels[state] ?? state;
}

function lifecycleTone(state: ModuleHealthState): "good" | "warn" | "danger" | "muted" {
  if (state === "available") return "good";
  if (["discovered", "installing", "starting", "degraded", "backoff"].includes(state)) return "warn";
  if (state === "stopped") return "muted";
  return "danger";
}

function configurationText(configuration: ModuleConfiguration | undefined): string {
  return JSON.stringify(configuration?.value ?? {}, null, 2);
}

function ModuleManagementCard({ module }: { module: PlatformModule }) {
  const queryClient = useQueryClient();
  const configurationQuery = useQuery({
    queryKey: platformQueryKeys.configuration(module.id),
    queryFn: () => platformApi.moduleConfiguration(module.id),
  });
  const [draft, setDraft] = useState("{}");
  const [confirmedSecretReplacement, setConfirmedSecretReplacement] = useState(false);

  useEffect(() => {
    if (!configurationQuery.data) return;
    setDraft(configurationText(configurationQuery.data));
    setConfirmedSecretReplacement(false);
  }, [configurationQuery.data]);

  const parsedDraft = useMemo(() => parseConfigurationDraft(draft), [draft]);
  const responseHasRedactedSecret = containsRedactedSecret(configurationQuery.data?.value ?? null);
  const draftHasRedactedSecret = parsedDraft.value !== null && containsRedactedSecret(parsedDraft.value);

  const saveMutation = useMutation({
    mutationKey: moduleConfigurationMutationKey(module.id),
    mutationFn: (value: JsonValue) => platformApi.saveModuleConfiguration(module.id, value),
    onSuccess: async (configuration) => {
      queryClient.setQueryData(platformQueryKeys.configuration(module.id), configuration);
      await queryClient.invalidateQueries({ queryKey: platformQueryKeys.modules });
    },
    onSettled: (_configuration, _error, variables) => {
      removeMutationFromCache(queryClient, moduleConfigurationMutationKey(module.id), variables);
    },
  });
  const lifecycleMutation = useMutation({
    mutationFn: (action: "enable" | "disable") => action === "enable"
      ? platformApi.enableModule(module.id)
      : platformApi.disableModule(module.id),
    onSuccess: (catalog) => {
      queryClient.setQueryData(platformQueryKeys.modules, catalog);
    },
  });

  const canSave = configurationQuery.isSuccess
    && parsedDraft.value !== null
    && !draftHasRedactedSecret
    && (!responseHasRedactedSecret || confirmedSecretReplacement)
    && !saveMutation.isPending;
  const canEnable = configurationQuery.data?.configured === true && !lifecycleMutation.isPending;
  const processText = module.pid === null ? "—" : String(module.pid);

  return (
    <article className="module-admin-card" aria-label={`模块 ${module.display_name}`}>
      <header className="module-admin-card__header">
        <div>
          <h3>{module.display_name}</h3>
          <p className="module-admin-card__identity"><code>{module.id}</code> · v{module.version}</p>
        </div>
        <span className={`module-state module-state--${lifecycleTone(module.lifecycle_state)}`}>
          {module.enabled ? lifecycleLabel(module.lifecycle_state) : "未启用"}
        </span>
      </header>

      <dl className="module-runtime-details">
        <div><dt>发行状态</dt><dd>已包含</dd></div>
        <div><dt>配置状态</dt><dd>{configurationQuery.isLoading ? "读取中…" : configurationQuery.data?.configured ? "已配置" : "未配置"}</dd></div>
        <div><dt>PID</dt><dd>{processText}</dd></div>
        <div><dt>重启次数</dt><dd>{module.restart_count}</dd></div>
      </dl>
      <p className="module-health-message" title={module.health_message}>{module.health_message}</p>

      {configurationQuery.error ? (
        <InlineNotice tone="danger" text={`配置读取失败：${configurationQuery.error.message}`} />
      ) : null}
      {configurationQuery.data?.validation_error ? (
        <InlineNotice
          tone="warn"
          text={`旧配置与当前模块版本不兼容，请按当前 Schema 提交完整新配置：${configurationQuery.data.validation_error}`}
        />
      ) : null}
      {lifecycleMutation.error ? (
        <InlineNotice tone="danger" text={`模块操作失败：${lifecycleMutation.error.message}`} />
      ) : null}
      {saveMutation.error ? (
        <InlineNotice tone="danger" text={`配置保存失败：${saveMutation.error.message}`} />
      ) : null}

      <div className="module-lifecycle-actions">
        {module.enabled ? (
          <button
            type="button"
            className="action-button danger"
            disabled={lifecycleMutation.isPending}
            onClick={() => lifecycleMutation.mutate("disable")}
          >
            {lifecycleMutation.isPending ? <Loader2 className="spin" size={16} /> : <Square size={16} />}
            <span>{lifecycleMutation.isPending ? "正在停用…" : "停用模块"}</span>
          </button>
        ) : (
          <button
            type="button"
            className="action-button primary"
            disabled={!canEnable}
            title={canEnable ? "启用模块" : "保存有效配置后才能启用"}
            onClick={() => lifecycleMutation.mutate("enable")}
          >
            {lifecycleMutation.isPending ? <Loader2 className="spin" size={16} /> : <Play size={16} />}
            <span>{lifecycleMutation.isPending ? "正在启用…" : "启用模块"}</span>
          </button>
        )}
      </div>

      <div className="module-configuration-editor">
        <div className="module-configuration-editor__heading">
          <h4>模块配置</h4>
          <span>Schema v{configurationQuery.data?.schema_version ?? "—"}</span>
        </div>
        <textarea
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          aria-label={`${module.display_name} 配置 JSON`}
          aria-invalid={parsedDraft.error !== null || draftHasRedactedSecret}
          autoComplete="off"
          spellCheck={false}
          disabled={!configurationQuery.isSuccess || saveMutation.isPending}
        />
        {parsedDraft.error ? <p className="module-config-validation" role="alert">{parsedDraft.error}</p> : null}
        {responseHasRedactedSecret ? (
          <div className="module-secret-warning" role="status">
            <p>服务器已将敏感字段显示为 <code>***</code>。保存前必须替换所有占位符，并提供完整的新配置值。</p>
            <label>
              <input
                type="checkbox"
                checked={confirmedSecretReplacement}
                onChange={(event) => setConfirmedSecretReplacement(event.target.checked)}
              />
              我已为所有隐藏字段填写完整的新值
            </label>
          </div>
        ) : null}
        {draftHasRedactedSecret ? (
          <p className="module-config-validation" role="alert">配置仍包含 ***，为避免覆盖原密钥，当前禁止保存。</p>
        ) : null}
        <div className="module-configuration-actions">
          <details>
            <summary>查看配置 Schema</summary>
            <pre>{JSON.stringify(configurationQuery.data?.schema ?? {}, null, 2)}</pre>
          </details>
          <button
            type="button"
            className="action-button primary"
            disabled={!canSave}
            onClick={() => {
              if (parsedDraft.value !== null) saveMutation.mutate(parsedDraft.value);
            }}
          >
            {saveMutation.isPending ? <Loader2 className="spin" size={16} /> : <Save size={16} />}
            <span>{saveMutation.isPending ? "正在保存…" : "保存配置"}</span>
          </button>
        </div>
      </div>
    </article>
  );
}

function ModuleManagementSection() {
  const queryClient = useQueryClient();
  const modulesQuery = useQuery({ queryKey: platformQueryKeys.modules, queryFn: platformApi.modules });
  const catalog = useMemo(() => modulesQuery.data === undefined
    ? { modules: [], issues: [] }
    : parseModuleCatalog(modulesQuery.data), [modulesQuery.data]);
  const rescanMutation = useMutation({
    mutationFn: platformApi.rescanModules,
    onSuccess: (latestCatalog) => {
      queryClient.setQueryData(platformQueryKeys.modules, latestCatalog);
    },
  });

  return (
    <section className="section-band module-management-section">
      <SectionHeader
        icon={Boxes}
        title="模块管理"
        description="发行构建决定包含哪些模块；此处只管理本发行内模块的配置与运行状态。"
        actions={(
          <button
            type="button"
            className="action-button primary"
            disabled={rescanMutation.isPending}
            onClick={() => rescanMutation.mutate()}
          >
            {rescanMutation.isPending ? <Loader2 className="spin" size={16} /> : <RefreshCw size={16} />}
            <span>{rescanMutation.isPending ? "正在重扫…" : "重新扫描"}</span>
          </button>
        )}
      />
      {modulesQuery.error ? <InlineNotice tone="danger" text={`模块目录读取失败：${modulesQuery.error.message}`} /> : null}
      {rescanMutation.error ? <InlineNotice tone="danger" text={`模块重扫失败：${rescanMutation.error.message}`} /> : null}
      {catalog.issues.map((issue) => (
        <InlineNotice key={issue.moduleId} tone="warn" text={`模块 ${issue.moduleId} Manifest 无效：${issue.message}`} />
      ))}
      {modulesQuery.isLoading ? <p className="module-management-empty">正在读取发行模块…</p> : null}
      {!modulesQuery.isLoading && !modulesQuery.error && catalog.modules.length === 0 ? (
        <p className="module-management-empty">当前发行未包含业务模块。</p>
      ) : null}
      <div className="module-management-grid">
        {catalog.modules.map((module) => <ModuleManagementCard key={module.id} module={module} />)}
      </div>
    </section>
  );
}

export function SettingsView({ onPasswordChanged }: { onPasswordChanged: () => void }) {
  return (
    <section className="view-stack">
      <AccountSection onPasswordChanged={onPasswordChanged} />
      <ModuleManagementSection />
    </section>
  );
}
