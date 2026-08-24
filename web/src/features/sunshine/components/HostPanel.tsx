import { useId, useState } from "react";
import {
  AppWindow,
  Check,
  Edit2,
  KeyRound,
  RefreshCw,
  RotateCcw,
  Settings2,
  Users,
  Wrench,
  X,
} from "lucide-react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ActionButton,
  InlineNotice,
  LoadingBlock,
  MutationError,
  SectionHeader,
} from "../../../shared/components/ui";
import { sunshineApi as api } from "../api";
import { parseSunshineConfigDraft } from "../data";
import { removeMutationFromCache } from "../../../shared/lib/mutations";
import { activateTabFromKeyboard } from "../../../shared/lib/tabs";
import { sunshineQueryKeys as queryKeys } from "../queryKeys";
import type { SunshineHostInfo } from "../types";
import { AppsSection } from "./AppsSection";
import { ClientsSection } from "./ClientsSection";

type HostSection = "apps" | "clients" | "pairing" | "config" | "system";

const HOST_SECTIONS: Array<{
  key: HostSection;
  label: string;
  Icon: React.ComponentType<{ size?: number }>;
}> = [
  { key: "apps", label: "应用", Icon: AppWindow },
  { key: "clients", label: "客户端", Icon: Users },
  { key: "pairing", label: "配对", Icon: KeyRound },
  { key: "config", label: "配置", Icon: Settings2 },
  { key: "system", label: "系统", Icon: Wrench },
];

function pairMutationKey(hostId: string) {
  return ["sunshine-pair", hostId] as const;
}

interface PairVariables {
  pin: string;
  deviceName: string;
}

function PairingSection({ host }: { host: SunshineHostInfo }) {
  const queryClient = useQueryClient();
  const [pin, setPin] = useState("");
  const [deviceName, setDeviceName] = useState("");
  const pairMutation = useMutation({
    mutationKey: pairMutationKey(host.id),
    mutationFn: ({ pin: submittedPin, deviceName: submittedDeviceName }: PairVariables) =>
      api.sunshinePin(host.id, submittedPin, submittedDeviceName),
    onSuccess: () => { setPin(""); setDeviceName(""); },
    onSettled: (_result, _error, variables) => {
      variables.pin = "";
      removeMutationFromCache(queryClient, pairMutationKey(host.id), variables);
    },
  });
  const canPair = /^\d{4,8}$/.test(pin.trim()) && !pairMutation.isPending;
  const submitPairing = () => pairMutation.mutate({
    pin: pin.trim(),
    deviceName: deviceName.trim() || "Moonlight Client",
  });

  return (
    <section className="section-band">
      <SectionHeader icon={KeyRound} title="PIN 配对" />
      <MutationError mutation={pairMutation} />
      {pairMutation.isSuccess ? <InlineNotice tone="warn" text="配对请求已提交。" /> : null}
      <div className="sunshine-pin-form">
        <label className="inline-field"><span>PIN 码 *</span>
          <input
            value={pin}
            onChange={(event) => {
              setPin(event.target.value);
              if (pairMutation.isSuccess || pairMutation.isError) pairMutation.reset();
            }}
            maxLength={8}
            minLength={4}
            inputMode="numeric"
            pattern="[0-9]{4,8}"
            placeholder="1234"
            autoFocus
            onKeyDown={(event) => {
              if (event.key === "Enter" && canPair) {
                event.preventDefault();
                submitPairing();
              }
            }}
          />
        </label>
        <label className="inline-field"><span>设备名称</span>
          <input value={deviceName} maxLength={80} onChange={(event) => setDeviceName(event.target.value)} placeholder="Moonlight Client" />
        </label>
        <div style={{ display: "flex" }}>
          <ActionButton icon={Check} label="提交配对" busy={pairMutation.isPending} disabled={!canPair} onClick={submitPairing} />
        </div>
      </div>
    </section>
  );
}

function ConfigSection({ host }: { host: SunshineHostInfo }) {
  const queryClient = useQueryClient();
  const queryKey = queryKeys.sunshine.config(host.id);
  const query = useQuery({ queryKey, queryFn: () => api.sunshineConfig(host.id) });
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

  const saveMutation = useMutation({
    mutationFn: () => api.sunshineSaveConfig(host.id, parseSunshineConfigDraft(draft ?? "{}")),
    onSuccess: async () => {
      setDraft(null);
      await queryClient.invalidateQueries({ queryKey });
    },
  });
  const entries = Object.entries(query.data ?? {});
  const cancelEdit = () => {
    setDraft(null);
    saveMutation.reset();
  };

  return (
    <section className="section-band">
      <SectionHeader
        icon={Settings2}
        title="配置"
        actions={editMode ? (
          <div className="button-row">
            <ActionButton icon={Check} label="保存" busy={saveMutation.isPending} disabled={!parsedDraft} onClick={() => saveMutation.mutate()} />
            <ActionButton icon={X} label="取消" onClick={cancelEdit} />
          </div>
        ) : (
          <ActionButton icon={Edit2} label="编辑 JSON" disabled={!query.data} onClick={() => setDraft(JSON.stringify(query.data ?? {}, null, 2))} />
        )}
      />
      {query.isLoading ? <LoadingBlock label="读取配置" /> : null}
      {query.error ? <InlineNotice tone="danger" text={query.error.message} /> : null}
      <MutationError mutation={saveMutation} />
      {!editMode ? (
        <div className="sunshine-config-table" aria-label="Sunshine 配置只读预览">
          {entries.map(([key, value]) => (
            <div className="sunshine-config-row" key={key}>
              <span className="mono">{key}</span>
              <span className="mono sunshine-config-value">
                {typeof value === "string" ? value : JSON.stringify(value)}
              </span>
            </div>
          ))}
        </div>
      ) : (
        <div className="sunshine-config-edit">
          <label className="inline-field wide">
            <span>完整 JSON 配置（保留字符串、数字、布尔值和对象类型）</span>
            <textarea
              className="sunshine-config-json"
              value={draft ?? ""}
              onChange={(event) => setDraft(event.target.value)}
              rows={20}
              spellCheck={false}
              aria-invalid={Boolean(draftError)}
            />
          </label>
          {draftError ? <InlineNotice tone="danger" text={draftError} /> : null}
        </div>
      )}
    </section>
  );
}

function SystemSection({ host }: { host: SunshineHostInfo }) {
  const restartMutation = useMutation({ mutationFn: () => api.sunshineRestart(host.id) });
  const resetMutation = useMutation({ mutationFn: () => api.sunshineResetDisplay(host.id) });

  return (
    <section className="view-stack">
      <section className="section-band">
        <SectionHeader icon={Wrench} title="系统操作" />
        <MutationError mutation={restartMutation} />
        <MutationError mutation={resetMutation} />
        {restartMutation.isSuccess ? <InlineNotice tone="warn" text="重启命令已发送。" /> : null}
        {resetMutation.isSuccess ? <InlineNotice tone="warn" text="显示设备配置已重置。" /> : null}
        <div className="sunshine-system-actions">
          <div className="sunshine-system-card">
            <RefreshCw size={24} />
            <div><strong>重启 Sunshine</strong><p>重新加载配置，当前串流会话将中断。</p></div>
            <ActionButton
              icon={RefreshCw}
              label="立即重启"
              tone="danger"
              busy={restartMutation.isPending}
              onClick={() => window.confirm("确定重启 Sunshine？当前会话将中断。") && restartMutation.mutate()}
            />
          </div>
          <div className="sunshine-system-card">
            <RotateCcw size={24} />
            <div><strong>重置显示设备</strong><p>清除 Sunshine 保存的显示设备持久化配置。</p></div>
            <ActionButton
              icon={RotateCcw}
              label="重置显示"
              busy={resetMutation.isPending}
              onClick={() => window.confirm("确定重置显示设备配置？") && resetMutation.mutate()}
            />
          </div>
        </div>
      </section>
    </section>
  );
}

export function HostPanel({
  host,
  onClose,
}: {
  host: SunshineHostInfo;
  onClose: () => void;
}) {
  const [section, setSection] = useState<HostSection>("apps");
  const tabsId = useId();

  return (
    <div className="sunshine-host-panel">
      <div className="sunshine-panel-nav-row">
        <nav className="sunshine-subnav-inline" role="tablist" aria-label={`${host.name} 管理功能`}>
          {HOST_SECTIONS.map(({ key, label, Icon }, index) => (
            <button
              key={key}
              type="button"
              id={`${tabsId}-tab-${key}`}
              role="tab"
              aria-selected={section === key}
              aria-controls={`${tabsId}-panel-${key}`}
              tabIndex={section === key ? 0 : -1}
              className={section === key ? "sunshine-section-tab active" : "sunshine-section-tab"}
              onClick={() => setSection(key)}
              onKeyDown={(event) => activateTabFromKeyboard(
                event,
                HOST_SECTIONS,
                index,
                (next) => setSection(next.key),
              )}
            >
              <Icon size={18} /><strong>{label}</strong>
            </button>
          ))}
        </nav>
        <button
          type="button"
          className="icon-button sunshine-panel-close"
          aria-label="关闭管理面板"
          title="关闭"
          autoFocus
          onClick={onClose}
        >
          <X size={18} aria-hidden="true" />
        </button>
      </div>
      {HOST_SECTIONS.map(({ key }) => (
        <div
          key={key}
          role="tabpanel"
          id={`${tabsId}-panel-${key}`}
          aria-labelledby={`${tabsId}-tab-${key}`}
          hidden={section !== key}
        >
          {section === "apps" && key === "apps" ? <AppsSection host={host} /> : null}
          {section === "clients" && key === "clients" ? <ClientsSection host={host} /> : null}
          {section === "pairing" && key === "pairing" ? <PairingSection host={host} /> : null}
          {section === "config" && key === "config" ? <ConfigSection host={host} /> : null}
          {section === "system" && key === "system" ? <SystemSection host={host} /> : null}
        </div>
      ))}
    </div>
  );
}
