import { useState } from "react";
import { AppWindow, Check, Edit2, Plus, Trash2, X } from "lucide-react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ActionButton,
  InlineNotice,
  LoadingBlock,
  MutationError,
  SectionHeader,
} from "../../../shared/components/ui";
import { sunshineApi as api } from "../api";
import { sunshineQueryKeys as queryKeys } from "../queryKeys";
import type { SunshineApp, SunshineAppsResponse, SunshineHostInfo } from "../types";

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
  const workingDirectory = typeof app["working-dir"] === "string" ? app["working-dir"] : "";
  const autoDetach = typeof app["auto-detach"] === "boolean" ? app["auto-detach"] : true;
  const waitAll = typeof app["wait-all"] === "boolean" ? app["wait-all"] : true;
  const exitTimeout = typeof app["exit-timeout"] === "number" ? app["exit-timeout"] : 5;

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
  // Sunshine's GET /api/apps uses the array position as the application id.
  return data.apps.map((app, index) => ({ ...app, index }));
}

export function AppsSection({ host }: { host: SunshineHostInfo }) {
  const queryClient = useQueryClient();
  const queryKey = queryKeys.sunshine.apps(host.id);
  const appsQuery = useQuery({
    queryKey,
    queryFn: () => api.sunshineApps(host.id),
    retry: false,
  });
  const [draft, setDraft] = useState<AppDraft | null>(null);

  const saveMutation = useMutation({
    mutationFn: (app: Partial<SunshineApp>) => api.sunshineSaveApp(host.id, app),
    onSuccess: async () => {
      setDraft(null);
      await queryClient.invalidateQueries({ queryKey });
    },
  });
  const deleteMutation = useMutation({
    mutationFn: (index: number) => api.sunshineDeleteApp(host.id, index),
    onSuccess: async () => { await queryClient.invalidateQueries({ queryKey }); },
  });
  const closeMutation = useMutation({
    mutationFn: () => api.sunshineCloseApp(host.id),
    onSuccess: async () => { await queryClient.invalidateQueries({ queryKey }); },
  });
  const apps = extractApps(appsQuery.data);

  return (
    <section className="section-band">
      <SectionHeader
        icon={AppWindow}
        title="应用"
        actions={(
          <div className="button-row">
            <ActionButton
              icon={X}
              label="结束会话"
              tone="danger"
              busy={closeMutation.isPending}
              onClick={() => window.confirm("结束当前应用会话？") && closeMutation.mutate()}
            />
            <ActionButton
              icon={Plus}
              label="新建"
              onClick={() => setDraft({
                name: "",
                cmd: "",
                "working-dir": "",
                "auto-detach": true,
                "wait-all": true,
                "exit-timeout": 5,
                index: -1,
              })}
            />
          </div>
        )}
      />
      <MutationError mutation={saveMutation} />
      <MutationError mutation={deleteMutation} />
      <MutationError mutation={closeMutation} />
      {draft ? (
        <div className="sunshine-app-form">
          <div className="sunshine-form-header">
            <strong>{draft.index === -1 ? "新建应用" : "编辑应用"}</strong>
            <button className="icon-button" type="button" aria-label="关闭应用编辑器" title="关闭" onClick={() => setDraft(null)}>
              <X size={16} aria-hidden="true" />
            </button>
          </div>
          <div className="sunshine-form-grid">
            <label className="inline-field wide"><span>名称 *</span>
              <input value={draft.name} onChange={(event) => setDraft((value) => value && { ...value, name: event.target.value })} autoFocus />
            </label>
            <label className="inline-field wide"><span>启动命令</span>
              <input value={draft.cmd} onChange={(event) => setDraft((value) => value && { ...value, cmd: event.target.value })} placeholder="留空=桌面串流" />
            </label>
            <label className="inline-field"><span>工作目录</span>
              <input value={draft["working-dir"]} onChange={(event) => setDraft((value) => value && { ...value, "working-dir": event.target.value })} />
            </label>
            <label className="inline-field"><span>退出超时（秒）</span>
              <input type="number" min={0} value={draft["exit-timeout"]} onChange={(event) => setDraft((value) => value && { ...value, "exit-timeout": Number(event.target.value) })} />
            </label>
          </div>
          <div className="button-row">
            <ActionButton
              icon={Check}
              label="保存"
              busy={saveMutation.isPending}
              disabled={!draft.name.trim() || !Number.isFinite(draft["exit-timeout"]) || draft["exit-timeout"] < 0}
              onClick={() => saveMutation.mutate({ ...draft, name: draft.name.trim() })}
            />
            <ActionButton icon={X} label="取消" onClick={() => setDraft(null)} />
          </div>
        </div>
      ) : null}
      {appsQuery.isLoading ? <LoadingBlock label="读取应用" /> : null}
      {appsQuery.error ? <InlineNotice tone="danger" text={appsQuery.error.message} /> : null}
      <div className="sunshine-app-list">
        {apps.map((app) => (
          <div className="sunshine-app-item" key={String(app.index)}>
            <div className="sunshine-app-info">
              <strong>{app.name}</strong>
              <span className="mono">{app.cmd || "（桌面串流）"}</span>
              <em>index: {app.index}</em>
            </div>
            <div className="button-row">
              <button className="icon-button" type="button" title="编辑" aria-label={`编辑应用 ${app.name}`} onClick={() => setDraft(appDraft(app))}>
                <Edit2 size={15} aria-hidden="true" />
              </button>
              <button
                className="icon-button danger"
                type="button"
                title="删除"
                disabled={deleteMutation.isPending}
                aria-label={`删除应用 ${app.name}`}
                onClick={() => window.confirm(`删除应用 "${app.name}"？`) && deleteMutation.mutate(app.index)}
              >
                <Trash2 size={15} />
              </button>
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}
