// Sunshine 多主机管理视图。
//
// 左侧：主机列表（增删改）。
// 右侧：选中主机的详细管理（5 个功能 tab）。

import { useEffect, useId, useRef, useState } from "react";
import {
  AppWindow,
  Boxes,
  Check,
  Edit2,
  ExternalLink,
  KeyRound,
  Plus,
  RefreshCw,
  RotateCcw,
  Settings2,
  ToggleLeft,
  ToggleRight,
  Trash2,
  Unlink,
  Users,
  Wrench,
  X
} from "lucide-react";
import { useMutation, useMutationState, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../api";
import { queryKeys } from "../query-keys";
import { querySunshineHosts } from "../sunshine-host-query";
import type {
  SunshineApp,
  SunshineAppsResponse,
  SunshineClient,
  SunshineClientsResponse,
  SunshineHostInfo,
  SunshineHostPatchRequest,
  SunshineHostSaveRequest
} from "../types";
import {
  isOptimisticSunshineHost,
  applySunshineHostPatch,
  optimisticSunshineHost,
  parseSunshineConfigDraft,
  removeSunshineHost,
  replaceSunshineHost,
  restoreSunshineHost,
  sunshineHostsRefetchInterval,
  sunshineHostMutationKeys,
} from "../sunshine-data";
import {
  ActionButton,
  CardActions,
  CardInner,
  CardRow,
  ContentTitle,
  InlineNotice,
  LoadingBlock,
  MutationError,
  SectionHeader,
  StatusLed
} from "../components/ui";

// ─── 类型 ─────────────────────────────────────────────────────────────────────

type HostSection = "apps" | "clients" | "pairing" | "config" | "system";

const HOST_SECTIONS: Array<{ key: HostSection; label: string; Icon: React.ComponentType<{ size?: number }> }> = [
  { key: "apps",    label: "应用",   Icon: AppWindow },
  { key: "clients", label: "客户端", Icon: Users     },
  { key: "pairing", label: "配对",   Icon: KeyRound  },
  { key: "config",  label: "配置",   Icon: Settings2 },
  { key: "system",  label: "系统",   Icon: Wrench    }
];

const RE_IPV4       = /^((25[0-5]|2[0-4]\d|1\d{2}|[1-9]\d|\d)\.){3}(25[0-5]|2[0-4]\d|1\d{2}|[1-9]\d|\d)$/;
const RE_DOMAIN     = /^(?!-)[A-Za-z0-9-]{1,63}(?<!-)(\.[A-Za-z0-9-]{1,63}(?<!-))*\.?$/;

// 手写的 IPv6 正则很难覆盖 `::` 压缩、内嵌 IPv4 等合法写法，又会放行 `::::` 之类的
// 畸形串。浏览器的 URL 解析器对方括号内的 IPv6 做的是完整校验，直接借用它——
// 这只是前端的即时提示，最终仍由后端 `is_valid_host` 权威裁定。
function isValidIpv6(v: string): boolean {
  const inner = v.startsWith("[") && v.endsWith("]") ? v.slice(1, -1) : v;
  if (!inner.includes(":")) return false;
  try {
    return new URL(`http://[${inner}]/`).hostname.startsWith("[");
  } catch {
    return false;
  }
}

function isValidHost(v: string): boolean {
  return RE_IPV4.test(v) || isValidIpv6(v) || RE_DOMAIN.test(v);
}

// ─── 主机卡片（article + 底部三按钮） ────────────────────────────────────────

export function InlineHostField({
  value,
  label,
  validate,
  onSave,
  compact = false,
  displayValue,
  inputType = "text",
  normalize = (next) => next.trim(),
  cancelEmpty = false,
  maxLength,
  disabled = false,
}: {
  value: string;
  label: string;
  validate: (value: string) => string | null;
  onSave: (value: string) => Promise<void>;
  compact?: boolean;
  displayValue?: string;
  inputType?: "text" | "password";
  normalize?: (value: string) => string;
  cancelEmpty?: boolean;
  maxLength?: number;
  disabled?: boolean;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(value);
  const [error, setError] = useState("");
  const errorId = useId();
  const committingRef = useRef(false);
  const skipBlurRef = useRef(false);

  const cancel = () => {
    skipBlurRef.current = true;
    setDraft(value);
    setError("");
    setEditing(false);
  };

  const commit = async () => {
    if (committingRef.current) return;
    const next = normalize(draft);
    if (cancelEmpty && next.length === 0) {
      setDraft(value);
      setError("");
      setEditing(false);
      return;
    }
    const validationError = validate(next);
    if (validationError) {
      setError(validationError);
      return;
    }
    if (next === value) {
      setEditing(false);
      return;
    }
    committingRef.current = true;
    try {
      await onSave(next);
      setError("");
      setEditing(false);
    } catch (saveError) {
      setError(saveError instanceof Error ? saveError.message : "保存失败");
    } finally {
      committingRef.current = false;
    }
  };

  if (editing) {
    return <>
      <input
          className={`sunshine-inline-input${compact ? " compact" : ""}${error ? " input-error" : ""}`}
          value={draft}
          type={inputType}
          aria-label={label}
          aria-invalid={Boolean(error)}
          aria-errormessage={error ? errorId : undefined}
          title={error || undefined}
          maxLength={maxLength}
          autoFocus
          onClick={(event) => event.stopPropagation()}
          onChange={(event) => { setDraft(event.target.value); setError(""); }}
          onBlur={() => {
            if (skipBlurRef.current) {
              skipBlurRef.current = false;
              return;
            }
            void commit();
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter") { event.preventDefault(); void commit(); }
            if (event.key === "Escape") { event.preventDefault(); cancel(); }
          }}
        />
      {error ? <span className="sr-only" id={errorId} role="alert">{error}</span> : null}
    </>;
  }

  return (
    <button
      type="button"
      className={`sunshine-inline-editable${compact ? " compact" : ""}`}
      title={disabled ? "正在保存主机，请稍候" : `修改${label}`}
      aria-label={`修改${label}，当前值：${displayValue ?? value}`}
      disabled={disabled}
      onClick={(event) => {
        event.stopPropagation();
        if (disabled) return;
        skipBlurRef.current = false;
        setDraft(value);
        setEditing(true);
      }}
    >
      {displayValue ?? value}
    </button>
  );
}

function HostCard({ host, selected, updating, onOpen, onDelete, onInlineUpdate }: {
  host: SunshineHostInfo;
  selected: boolean;
  updating: boolean;
  onOpen: () => void;
  onDelete: () => void;
  onInlineUpdate: (patch: SunshineHostPatchRequest) => Promise<void>;
}) {
  const probePending = host.probe_status === "pending";
  const optimistic = isOptimisticSunshineHost(host);
  const controlsDisabled = optimistic || updating;
  const connectionLabel = probePending
    ? (host.connection_error ?? "正在检测 Sunshine 连接")
    : host.connected ? "Sunshine API 已连接" : (host.connection_error ?? "Sunshine API 未连接");

  return (
    <article
      className={`content-card service-card sunshine-host-card${selected ? " active" : ""}`}
      aria-busy={controlsDisabled}
      aria-label={`${host.name}，${connectionLabel}`}
    >
      <CardInner>
        <CardRow label="名称">
          <InlineHostField
            label="名称"
            value={host.name}
            validate={(value) => value && value.length <= 128 ? null : "名称必须为 1–128 个字符"}
            onSave={(name) => onInlineUpdate({ name })}
            maxLength={128}
            disabled={controlsDisabled}
          />
          <span title={connectionLabel}>
            <StatusLed tone={probePending ? "warn" : host.connected ? "good" : "danger"} />
            <span className="sr-only">{connectionLabel}</span>
          </span>
        </CardRow>
        <CardRow label="地址">
          <div className="card-address-inline">
            <InlineHostField
              label="地址"
              value={host.host}
              validate={(value) => isValidHost(value) ? null : "请输入有效的 IPv4、IPv6 或域名"}
              onSave={(address) => onInlineUpdate({ host: address })}
              maxLength={253}
              disabled={controlsDisabled}
            />
            <span className="sunshine-inline-separator">:</span>
            <InlineHostField
              label="端口"
              value={String(host.web_port)}
              compact
              validate={(value) => {
                const port = Number(value);
                return Number.isInteger(port) && port >= 1 && port <= 65535 ? null : "端口必须是 1–65535 的整数";
              }}
              onSave={(port) => onInlineUpdate({ web_port: Number(port) })}
              disabled={controlsDisabled}
            />
          </div>
        </CardRow>
        <CardRow label="账号">
          <InlineHostField label="账号" value={host.username}
            validate={value => value && value.length <= 256 ? null : "账号必须为 1–256 个字符"}
            onSave={username => onInlineUpdate({ username })} maxLength={256} disabled={controlsDisabled} />
        </CardRow>
        <CardRow label="密码">
          <InlineHostField label="密码" value="" displayValue={host.password_set ? "已设置" : "未设置"} inputType="password"
            validate={value => value.length <= 4096 ? null : "密码不能超过 4096 个字符"}
            onSave={password => onInlineUpdate({ password })} normalize={value => value} cancelEmpty maxLength={4096} disabled={controlsDisabled} />
          {host.password_set ? (
            <button
              type="button"
              className="card-action-button danger"
              disabled={controlsDisabled}
              aria-label={`清空 ${host.name} 的 Sunshine 密码`}
              title="清空密码"
              onClick={() => window.confirm("确定清空该 Sunshine 主机的密码？") && void onInlineUpdate({ password: "" })}
            >清空</button>
          ) : null}
        </CardRow>
        <CardRow label="TLS">
          <button type="button" className="card-action-button" disabled={controlsDisabled}
            title={controlsDisabled ? "正在保存主机，请稍候" : "仅开发模式允许关闭证书验证；生产模式会拒绝此操作"}
            onClick={() => {
              if (!host.verify_tls || window.confirm("仅开发模式允许关闭 TLS 证书验证；生产模式会拒绝。仍要尝试吗？")) {
                void onInlineUpdate({ verify_tls: !host.verify_tls });
              }
            }}>
            {host.verify_tls ? "验证证书" : "允许自签名"}
          </button>
        </CardRow>
        <CardActions>
            <button type="button" className="card-action-button" disabled={controlsDisabled} onClick={onOpen}>
              <Edit2 size={12} /><span>{selected ? "收起管理" : "管理"}</span>
            </button>
            <button type="button" className="card-action-button danger" disabled={controlsDisabled}
              onClick={onDelete}>
              <Trash2 size={12} /><span>删除</span>
            </button>
            <a href={controlsDisabled ? undefined : host.web_url} target="_blank" rel="noopener noreferrer"
              className="card-action-button primary"
              aria-disabled={controlsDisabled}
              tabIndex={controlsDisabled ? -1 : undefined}
              onClick={(event) => {
                if (controlsDisabled) event.preventDefault();
              }}>
              <ExternalLink size={12} /><span>打开</span>
            </a>
        </CardActions>
      </CardInner>
    </article>
  );
}

// ─── 已选主机的管理面板 ───────────────────────────────────────────────────────

function HostPanel({ host }: { host: SunshineHostInfo }) {
  const [section, setSection] = useState<HostSection>("apps");
  const tabsId = useId();

  return (
    <div className="sunshine-host-panel">
      <div className="sunshine-panel-nav-row">
        <nav className="sunshine-subnav-inline" role="tablist" aria-label={`${host.name} 管理功能`}>
          {HOST_SECTIONS.map(({ key, label, Icon }) => (
            <button
              key={key}
              type="button"
              id={`${tabsId}-tab-${key}`}
              role="tab"
              aria-selected={section === key}
              aria-controls={`${tabsId}-panel-${key}`}
              className={section === key ? "sunshine-section-tab active" : "sunshine-section-tab"}
              onClick={() => setSection(key)}
            >
              <Icon size={18} /><strong>{label}</strong>
            </button>
          ))}
        </nav>
      </div>

      <div role="tabpanel" id={`${tabsId}-panel-${section}`} aria-labelledby={`${tabsId}-tab-${section}`}>
        {section === "apps" && <AppsSection host={host} />}
        {section === "clients" && <ClientsSection host={host} />}
        {section === "pairing" && <PairingSection host={host} />}
        {section === "config" && <ConfigSection host={host} />}
        {section === "system" && <SystemSection host={host} />}
      </div>
    </div>
  );
}

// ─── 应用 tab ─────────────────────────────────────────────────────────────────

type AppDraft = SunshineApp & {
  name: string;
  cmd: string;
  "working-dir": string;
  "auto-detach": boolean;
  "wait-all": boolean;
  "exit-timeout": number;
  index: number;
};

export function appDraft(app: SunshineApp): AppDraft {
  const workingDirectory = typeof app["working-dir"] === "string"
    ? app["working-dir"]
    : "";
  const autoDetach = typeof app["auto-detach"] === "boolean"
    ? app["auto-detach"]
    : true;
  const waitAll = typeof app["wait-all"] === "boolean"
    ? app["wait-all"]
    : true;
  const exitTimeout = typeof app["exit-timeout"] === "number"
    ? app["exit-timeout"]
    : 5;

  return {
    ...app,
    name: typeof app.name === "string" ? app.name : "",
    cmd: typeof app.cmd === "string" ? app.cmd : "",
    "working-dir": workingDirectory,
    "auto-detach": autoDetach,
    "wait-all": waitAll,
    "exit-timeout": exitTimeout,
    index: app.index,
  };
}

function extractApps(data: SunshineAppsResponse | undefined): SunshineApp[] {
  if (!data) return [];
  // Sunshine 的 GET /api/apps 返回数组位置作为应用 ID，条目本身通常没有 index。
  return data.apps.map((app, index) => ({ ...app, index }));
}

function AppsSection({ host }: { host: SunshineHostInfo }) {
  const qc = useQueryClient();
  const qKey = queryKeys.sunshine.apps(host.id);
  const appsQuery = useQuery({
    queryKey: qKey,
    queryFn: () => api.sunshineApps(host.id),
    retry: false,
  });
  const [draft, setDraft] = useState<AppDraft | null>(null);

  const saveMutation = useMutation({
    mutationFn: (app: Partial<SunshineApp>) => api.sunshineSaveApp(host.id, app),
    onSuccess: async () => { setDraft(null); await qc.invalidateQueries({ queryKey: qKey }); }
  });
  const deleteMutation = useMutation({
    mutationFn: (idx: number) => api.sunshineDeleteApp(host.id, idx),
    onSuccess: async () => { await qc.invalidateQueries({ queryKey: qKey }); }
  });
  const closeMutation = useMutation({
    mutationFn: () => api.sunshineCloseApp(host.id),
    onSuccess: async () => { await qc.invalidateQueries({ queryKey: qKey }); }
  });

  const apps = extractApps(appsQuery.data);

  return (
    <section className="section-band">
      <SectionHeader icon={AppWindow} title="应用" actions={
        <div className="button-row">
          <ActionButton icon={X} label="结束会话" tone="danger" busy={closeMutation.isPending}
            onClick={() => window.confirm("结束当前应用会话？") && closeMutation.mutate()} />
          <ActionButton icon={Plus} label="新建" onClick={() =>
            setDraft({ name: "", cmd: "", "working-dir": "", "auto-detach": true, "wait-all": true, "exit-timeout": 5, index: -1 })} />
        </div>
      } />
      <MutationError mutation={saveMutation} />
      <MutationError mutation={deleteMutation} />
      <MutationError mutation={closeMutation} />
      {draft ? (
        <div className="sunshine-app-form">
          <div className="sunshine-form-header">
            <strong>{draft.index === -1 ? "新建应用" : "编辑应用"}</strong>
            <button className="icon-button" type="button" aria-label="关闭应用编辑器" title="关闭" onClick={() => setDraft(null)}><X size={16} aria-hidden="true" /></button>
          </div>
          <div className="sunshine-form-grid">
            <label className="inline-field wide"><span>名称 *</span>
              <input value={draft.name} onChange={e => setDraft(d => d && { ...d, name: e.target.value })} autoFocus /></label>
            <label className="inline-field wide"><span>启动命令</span>
              <input value={draft.cmd} onChange={e => setDraft(d => d && { ...d, cmd: e.target.value })} placeholder="留空=桌面串流" /></label>
            <label className="inline-field"><span>工作目录</span>
              <input value={draft["working-dir"]} onChange={e => setDraft(d => d && { ...d, "working-dir": e.target.value })} /></label>
            <label className="inline-field"><span>退出超时（秒）</span>
              <input type="number" min={0} value={draft["exit-timeout"]} onChange={e => setDraft(d => d && { ...d, "exit-timeout": Number(e.target.value) })} /></label>
          </div>
          <div className="button-row">
            <ActionButton icon={Check} label="保存" busy={saveMutation.isPending}
              disabled={!draft.name.trim() || !Number.isFinite(draft["exit-timeout"]) || draft["exit-timeout"] < 0}
              onClick={() => draft && saveMutation.mutate({ ...draft, name: draft.name.trim() })} />
            <ActionButton icon={X} label="取消" onClick={() => setDraft(null)} />
          </div>
        </div>
      ) : null}
      {appsQuery.isLoading ? <LoadingBlock label="读取应用" /> : null}
      {appsQuery.error ? <InlineNotice tone="danger" text={appsQuery.error.message} /> : null}
      <div className="sunshine-app-list">
        {apps.map((app) => (
          <div
            className="sunshine-app-item"
            key={String(app.index)}
          >
            <div className="sunshine-app-info">
              <strong>{app.name}</strong>
              <span className="mono">{(app.cmd as string) || "（桌面串流）"}</span>
              <em>index: {app.index}</em>
            </div>
            <div className="button-row">
              <button className="icon-button" type="button" title="编辑" aria-label={`编辑应用 ${app.name}`}
                onClick={() => setDraft(appDraft(app))}>
                <Edit2 size={15} aria-hidden="true" />
              </button>
              <button className="icon-button danger" type="button" title="删除" disabled={deleteMutation.isPending}
                aria-label={`删除应用 ${app.name}`}
                onClick={() => window.confirm(`删除应用 "${app.name}"？`) && deleteMutation.mutate(app.index)}>
                <Trash2 size={15} />
              </button>
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}

// ─── 客户端 tab ───────────────────────────────────────────────────────────────

function extractClients(data: SunshineClientsResponse | undefined): SunshineClient[] {
  return data ? data.named_certs : [];
}

function ClientsSection({ host }: { host: SunshineHostInfo }) {
  const qc = useQueryClient();
  const qKey = queryKeys.sunshine.clients(host.id);
  const query = useQuery({ queryKey: qKey, queryFn: () => api.sunshineClients(host.id) });

  const unpairM = useMutation({ mutationFn: (uuid: string) => api.sunshineUnpairClient(host.id, uuid),
    onSuccess: async () => qc.invalidateQueries({ queryKey: qKey }) });
  const unpairAllM = useMutation({ mutationFn: () => api.sunshineUnpairAll(host.id),
    onSuccess: async () => qc.invalidateQueries({ queryKey: qKey }) });
  const updateM = useMutation({ mutationFn: ({ uuid, enabled }: { uuid: string; enabled: boolean }) =>
    api.sunshineUpdateClient(host.id, uuid, enabled), onSuccess: async () => qc.invalidateQueries({ queryKey: qKey }) });

  const clients = extractClients(query.data);

  return (
    <section className="section-band">
      <SectionHeader icon={Users} title="客户端" actions={
        <ActionButton icon={Unlink} label="取消所有配对" tone="danger" busy={unpairAllM.isPending}
          onClick={() => window.confirm("取消所有配对？") && unpairAllM.mutate()} />
      } />
      <MutationError mutation={unpairM} />
      <MutationError mutation={unpairAllM} />
      <MutationError mutation={updateM} />
      {query.isLoading ? <LoadingBlock label="读取客户端" /> : null}
      {query.error ? <InlineNotice tone="danger" text={query.error.message} /> : null}
      <div className="sunshine-client-list">
        {clients.map(c => (
          <div className="sunshine-client-item" key={c.uuid}>
            <div className="sunshine-client-info">
              <strong>{c.name ?? "未命名设备"}</strong>
              <span className="mono">{c.uuid}</span>
              <span className="sunshine-client-status">
                <StatusLed tone={c.enabled ? "good" : "warn"} />
                {c.enabled ? "已启用" : "已禁用"}
              </span>
            </div>
            <div className="button-row">
              <button className="icon-button" type="button" title={c.enabled ? "禁用" : "启用"}
                aria-label={`${c.enabled ? "禁用" : "启用"}客户端 ${c.name ?? c.uuid}`}
                disabled={updateM.isPending} onClick={() => updateM.mutate({ uuid: c.uuid, enabled: !c.enabled })}>
                {c.enabled ? <ToggleRight size={18} /> : <ToggleLeft size={18} />}
              </button>
              <button className="icon-button danger" type="button" title="取消配对" disabled={unpairM.isPending}
                aria-label={`取消客户端 ${c.name ?? c.uuid} 的配对`}
                onClick={() => window.confirm(`取消设备 "${c.name ?? c.uuid}" 的配对？`) && unpairM.mutate(c.uuid)}>
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

// ─── 配对 tab ─────────────────────────────────────────────────────────────────

function PairingSection({ host }: { host: SunshineHostInfo }) {
  const [pin, setPin] = useState("");
  const [deviceName, setDeviceName] = useState("");
  const pairM = useMutation({
    mutationFn: () => api.sunshinePin(host.id, pin.trim(), deviceName.trim() || "Moonlight Client"),
    onSuccess: () => { setPin(""); setDeviceName(""); }
  });
  const canPair = /^\d{4,8}$/.test(pin.trim()) && !pairM.isPending;

  return (
    <section className="section-band">
      <SectionHeader icon={KeyRound} title="PIN 配对" />
      <MutationError mutation={pairM} />
      {pairM.isSuccess ? <InlineNotice tone="warn" text="配对请求已提交。" /> : null}
      <div className="sunshine-pin-form">
        <label className="inline-field"><span>PIN 码 *</span>
          <input value={pin} onChange={e => { setPin(e.target.value); if (pairM.isSuccess || pairM.isError) pairM.reset(); }}
            maxLength={8} minLength={4} inputMode="numeric" pattern="[0-9]{4,8}" placeholder="1234" autoFocus
            onKeyDown={e => {
              if (e.key === "Enter" && canPair) { e.preventDefault(); pairM.mutate(); }
            }} /></label>
        <label className="inline-field"><span>设备名称</span>
          <input value={deviceName} maxLength={80} onChange={e => setDeviceName(e.target.value)} placeholder="Moonlight Client" /></label>
        <div style={{ display: "flex" }}>
          <ActionButton icon={Check} label="提交配对" busy={pairM.isPending} disabled={!canPair} onClick={() => pairM.mutate()} />
        </div>
      </div>
    </section>
  );
}

// ─── 配置 tab ─────────────────────────────────────────────────────────────────

function ConfigSection({ host }: { host: SunshineHostInfo }) {
  const qc = useQueryClient();
  const qKey = queryKeys.sunshine.config(host.id);
  const query = useQuery({ queryKey: qKey, queryFn: () => api.sunshineConfig(host.id) });
  const [draft, setDraft] = useState<string | null>(null);
  const editMode = draft !== null;

  let parsedDraft = null;
  let draftError = "";
  if (draft !== null) {
    try {
      parsedDraft = parseSunshineConfigDraft(draft);
    } catch (error) {
      draftError = error instanceof Error ? error.message : "配置不是有效的 JSON 对象";
    }
  }

  const saveM = useMutation({
    mutationFn: () => api.sunshineSaveConfig(host.id, parseSunshineConfigDraft(draft ?? "{}")),
    onSuccess: async () => { setDraft(null); await qc.invalidateQueries({ queryKey: qKey }); }
  });

  const entries = Object.entries(query.data ?? {});
  const beginEdit = () => {
    setDraft(JSON.stringify(query.data ?? {}, null, 2));
  };
  const cancelEdit = () => {
    setDraft(null);
    saveM.reset();
  };

  return (
    <section className="section-band">
      <SectionHeader icon={Settings2} title="配置" actions={
        editMode ? (
          <div className="button-row">
            <ActionButton icon={Check} label="保存" busy={saveM.isPending} disabled={!parsedDraft}
              onClick={() => saveM.mutate()} />
            <ActionButton icon={X} label="取消" onClick={cancelEdit} />
          </div>
        ) : <ActionButton icon={Edit2} label="编辑 JSON" disabled={!query.data} onClick={beginEdit} />
      } />
      {query.isLoading ? <LoadingBlock label="读取配置" /> : null}
      {query.error ? <InlineNotice tone="danger" text={query.error.message} /> : null}
      <MutationError mutation={saveM} />
      {!editMode ? (
        <div className="sunshine-config-table" aria-label="Sunshine 配置只读预览">
          {entries.map(([k, v]) => (
            <div className="sunshine-config-row" key={k}>
              <span className="mono">{k}</span>
              <span className="mono sunshine-config-value">
                {typeof v === "string" ? v : JSON.stringify(v)}
              </span>
            </div>
          ))}
        </div>
      ) : (
        <div className="sunshine-config-edit">
          <label className="inline-field wide">
            <span>完整 JSON 配置（保留字符串、数字、布尔值和对象类型）</span>
            <textarea className="sunshine-config-json" value={draft ?? ""}
              onChange={event => setDraft(event.target.value)} rows={20} spellCheck={false}
              aria-invalid={Boolean(draftError)} />
          </label>
          {draftError ? <InlineNotice tone="danger" text={draftError} /> : null}
        </div>
      )}
    </section>
  );
}

// ─── 系统 tab ─────────────────────────────────────────────────────────────────

function SystemSection({ host }: { host: SunshineHostInfo }) {
  const restartM = useMutation({ mutationFn: () => api.sunshineRestart(host.id) });
  const resetM = useMutation({ mutationFn: () => api.sunshineResetDisplay(host.id) });

  return (
    <section className="view-stack">
      <section className="section-band">
        <SectionHeader icon={Wrench} title="系统操作" />
        <MutationError mutation={restartM} />
        <MutationError mutation={resetM} />
        {restartM.isSuccess ? <InlineNotice tone="warn" text="重启命令已发送。" /> : null}
        {resetM.isSuccess ? <InlineNotice tone="warn" text="显示设备配置已重置。" /> : null}
        <div className="sunshine-system-actions">
          <div className="sunshine-system-card">
            <RefreshCw size={24} />
            <div><strong>重启 Sunshine</strong><p>重新加载配置，当前串流会话将中断。</p></div>
            <ActionButton icon={RefreshCw} label="立即重启" tone="danger" busy={restartM.isPending}
              onClick={() => window.confirm("确定重启 Sunshine？当前会话将中断。") && restartM.mutate()} />
          </div>
          <div className="sunshine-system-card">
            <RotateCcw size={24} />
            <div><strong>重置显示设备</strong><p>清除 Sunshine 保存的显示设备持久化配置。</p></div>
            <ActionButton icon={RotateCcw} label="重置显示" busy={resetM.isPending}
              onClick={() => window.confirm("确定重置显示设备配置？") && resetM.mutate()} />
          </div>
        </div>
      </section>
    </section>
  );
}

// ─── SunshineView 根组件 ──────────────────────────────────────────────────────

export function SunshineView({ addTrigger = 0 }: { addTrigger?: number }) {
  const qc = useQueryClient();
  const createInFlightRef = useRef(false);
  const deletingHostIdsRef = useRef(new Set<string>());
  const hostsQuery = useQuery({
    queryKey: queryKeys.sunshine.hosts,
    queryFn: ({ signal }) => querySunshineHosts(qc, signal),
    // 新建/修改先返回 pending 快照；短轮询只持续到后台探测完成。
    refetchInterval: (query) => sunshineHostsRefetchInterval(
      query.state.data,
      deletingHostIdsRef.current.size > 0,
    ),
  });
  const hosts = hostsQuery.data ?? [];

  const [selectedId, setSelectedId] = useState<string | null>(null);
  const handledAddTriggerRef = useRef(addTrigger);

  const panelOpen = selectedId !== null;

  const createM = useMutation({
    mutationKey: sunshineHostMutationKeys.create,
    mutationFn: (req: SunshineHostSaveRequest) => api.sunshineCreateHost(req),
    onMutate: async (req) => {
      await qc.cancelQueries({ queryKey: queryKeys.sunshine.hosts, exact: true });
      const optimistic = optimisticSunshineHost(req);
      qc.setQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts, (current) => [
        ...(current ?? []),
        optimistic,
      ]);
      return { optimisticId: optimistic.id };
    },
    onSuccess: (saved, _req, context) => {
      qc.setQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts, (current) =>
        replaceSunshineHost(current ?? [], saved, context.optimisticId));
    },
    onError: (_error, _req, context) => {
      if (!context) return;
      qc.setQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts, (current) =>
        removeSunshineHost(current ?? [], context.optimisticId));
    },
    onSettled: () => {
      createInFlightRef.current = false;
      void qc.invalidateQueries({ queryKey: queryKeys.sunshine.hosts, exact: true });
    },
  });
  const updateM = useMutation({
    mutationKey: sunshineHostMutationKeys.update,
    mutationFn: ({ id, patch }: { id: string; patch: SunshineHostPatchRequest }) => api.sunshineUpdateHost(id, patch),
    onMutate: async ({ id, patch }) => {
      await qc.cancelQueries({ queryKey: queryKeys.sunshine.hosts, exact: true });
      const previous = qc.getQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts)
        ?.find((host) => host.id === id);
      if (previous) {
        qc.setQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts, (current) =>
          replaceSunshineHost(current ?? [], applySunshineHostPatch(previous, patch)));
      }
      return { previous };
    },
    onSuccess: (saved) => {
      qc.setQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts, (current) =>
        replaceSunshineHost(current ?? [], saved));
    },
    onError: (_error, { id }, context) => {
      const previous = context?.previous;
      if (!previous) return;
      qc.setQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts, (current) =>
        replaceSunshineHost(current ?? [], previous, id));
    },
    onSettled: () => {
      void qc.invalidateQueries({ queryKey: queryKeys.sunshine.hosts, exact: true });
    },
  });
  const pendingUpdateVariables = useMutationState<{ id: string; patch: SunshineHostPatchRequest }>({
    filters: {
      mutationKey: sunshineHostMutationKeys.update,
      exact: true,
      status: "pending",
    },
    select: (mutation) => mutation.state.variables as { id: string; patch: SunshineHostPatchRequest },
  });
  const updatingHostIds = new Set(pendingUpdateVariables.map(({ id }) => id));
  const deleteM = useMutation({
    mutationKey: sunshineHostMutationKeys.delete,
    mutationFn: (id: string) => api.sunshineDeleteHost(id),
    onMutate: async (id) => {
      await qc.cancelQueries({ queryKey: queryKeys.sunshine.hosts, exact: true });
      const current = qc.getQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts) ?? [];
      const originalIndex = current.findIndex((host) => host.id === id);
      const removed = originalIndex >= 0 ? current[originalIndex] : undefined;
      qc.setQueryData<SunshineHostInfo[]>(
        queryKeys.sunshine.hosts,
        removeSunshineHost(current, id),
      );
      if (selectedId === id) setSelectedId(null);
      return { originalIndex, removed };
    },
    onSuccess: (_result, id) => {
      // A late polling response must not be able to resurrect a confirmed deletion.
      qc.setQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts, (current) =>
        removeSunshineHost(current ?? [], id));
      qc.removeQueries({ queryKey: queryKeys.sunshine.apps(id), exact: true });
      qc.removeQueries({ queryKey: queryKeys.sunshine.clients(id), exact: true });
      qc.removeQueries({ queryKey: queryKeys.sunshine.config(id), exact: true });
      qc.removeQueries({ queryKey: queryKeys.logs.sunshine(id), exact: true });
    },
    onError: (_error, _id, context) => {
      if (!context?.removed) return;
      qc.setQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts, (current) =>
        restoreSunshineHost(current ?? [], context.removed!, context.originalIndex));
    },
    onSettled: (_result, _error, id) => {
      deletingHostIdsRef.current.delete(id);
      void qc.invalidateQueries({ queryKey: queryKeys.sunshine.hosts, exact: true });
    },
  });

  const selectedHost = hosts.find(h => h.id === selectedId) ?? null;

  function handleHostOpen(id: string) {
    if (selectedId === id) {
      setSelectedId(null);
    } else {
      setSelectedId(id);
    }
  }

  function createDefaultHost() {
    if (createInFlightRef.current) return;
    const usedNames = new Set(hosts.map(host => host.name));
    let index = hosts.length + 1;
    while (usedNames.has(`Sunshine ${index}`)) index += 1;
    createInFlightRef.current = true;
    createM.mutate({
      name: `Sunshine ${index}`,
      host: "192.168.1.2",
      web_port: 47990,
      username: "admin",
      password: null,
      verify_tls: true,
    });
    setSelectedId(null);
  }

  // 响应导航栏"+"按钮触发信号。只消费严格递增的触发序号；主机列表刷新
  // 不应让同一次点击再次创建实例。
  useEffect(() => {
    if (addTrigger <= handledAddTriggerRef.current) return;
    handledAddTriggerRef.current = addTrigger;
    createDefaultHost();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [addTrigger]);

  function deleteHost(id: string) {
    if (deletingHostIdsRef.current.has(id)) return;
    deletingHostIdsRef.current.add(id);
    deleteM.mutate(id);
  }

  return (
    <section className="view-stack">
      <section className="section-band sunshine-new-section">
        {/* mutation 错误提示 */}
        <MutationError mutation={createM} />
        <MutationError mutation={updateM} />
        <MutationError mutation={deleteM} />

        {hostsQuery.error ? <InlineNotice tone="danger" text={hostsQuery.error.message} /> : null}
        {hostsQuery.isLoading ? <LoadingBlock label="读取主机" /> : null}
        {!hostsQuery.isLoading && !hosts.length ? <p className="muted-inline">暂无主机，点击 + 新建</p> : null}

        {/* 响应式 master-detail：桌面并排，窄屏自然进入文档流。 */}
        <div className="instance-list-title"><ContentTitle icon={Boxes} title="实例" /></div>
        <div className={`sunshine-master-detail${panelOpen ? " has-panel" : ""}`}>
          <div className="content-grid sunshine-host-grid">
            {hosts.map(h => (
              <HostCard
                key={h.id}
                host={h}
                selected={selectedId === h.id}
                updating={updatingHostIds.has(h.id)}
                onOpen={() => {
                  if (!isOptimisticSunshineHost(h)) handleHostOpen(h.id);
                }}
                onInlineUpdate={(patch) => updateM.mutateAsync({ id: h.id, patch }).then(() => undefined)}
                onDelete={() => {
                  if (isOptimisticSunshineHost(h)) return;
                  if (window.confirm(`确定删除主机 "${h.name}"？`)) deleteHost(h.id);
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
