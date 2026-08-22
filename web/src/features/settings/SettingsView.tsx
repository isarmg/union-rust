import { useState } from "react";
import { Check, Edit2, KeyRound, Loader2, Save, X } from "lucide-react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { authApi as api } from "../auth/api";
import { authQueryKeys as queryKeys } from "../auth/queryKeys";
import { CardActions, CardInner, CardRow, InlineNotice, MutationError, SectionHeader, TruncatedText } from "../../shared/components/ui";

// ─── 修改密码侧面板 ───────────────────────────────────────────────────────────

function ChangePasswordPanel({ onClose }: { onClose: () => void }) {
  const [currentPw, setCurrentPw] = useState("");
  const [newPw, setNewPw] = useState("");
  const [confirmPw, setConfirmPw] = useState("");

  const changeMutation = useMutation({
    mutationFn: () => api.changePassword(currentPw, newPw),
    onSuccess: async () => {
      setCurrentPw("");
      setNewPw("");
      setConfirmPw("");
      try {
        await api.logout();
      } catch {
        // ignore
      }
      window.dispatchEvent(new Event("unionc:auth-expired"));
    }
  });

  const passwordMismatch = confirmPw.length > 0 && newPw !== confirmPw;
  const canSubmit =
    currentPw.length > 0 && newPw.length >= 12 && newPw === confirmPw && !changeMutation.isPending;

  return (
    <div className="settings-side-panel">
      <div className="settings-panel-header">
        <strong>修改密码</strong>
        <button className="icon-button" type="button" aria-label="关闭修改密码面板" title="关闭" onClick={onClose}><X size={16} aria-hidden="true" /></button>
      </div>
      <form
        className="account-form"
        onSubmit={(e) => { e.preventDefault(); if (newPw !== confirmPw) return; changeMutation.mutate(); }}
      >
        <label className="inline-field">
          <span>当前密码</span>
          <input type="password" value={currentPw} onChange={(e) => setCurrentPw(e.target.value)}
            autoComplete="current-password" placeholder="输入当前密码" autoFocus />
        </label>
        <label className="inline-field">
          <span>新密码</span>
          <input type="password" value={newPw} onChange={(e) => setNewPw(e.target.value)}
            autoComplete="new-password" placeholder="至少 12 个字符" />
        </label>
        <label className="inline-field">
          <span>确认新密码</span>
          <input type="password" value={confirmPw} onChange={(e) => setConfirmPw(e.target.value)}
            autoComplete="new-password" placeholder="再次输入新密码"
            className={passwordMismatch ? "input-error" : ""} />
        </label>
        {passwordMismatch && <InlineNotice tone="warn" text="两次输入的新密码不一致" />}
        <MutationError mutation={changeMutation} />
        {changeMutation.isSuccess && (
          <p className="account-success"><Check size={15} /> 密码已修改成功</p>
        )}
        <div className="settings-panel-actions">
          <button type="submit" className="action-button primary" disabled={!canSubmit}>
            {changeMutation.isPending ? <Loader2 size={16} className="spin" /> : <Save size={16} />}
            <span>修改密码</span>
          </button>
        </div>
      </form>
    </div>
  );
}

// ─── 账号管理区域 ─────────────────────────────────────────────────────────────

function AccountSection() {
  const meQuery = useQuery({ queryKey: queryKeys.me, queryFn: api.authenticate });
  const [panelOpen, setPanelOpen] = useState(false);

  return (
    <section className="section-band">
      <SectionHeader icon={KeyRound} title="账号管理" />
      {meQuery.error ? <InlineNotice tone="danger" text={`账号信息读取失败：${meQuery.error.message}`} /> : null}
      <div className="content-grid settings-grid">
        <div className="content-card setting-item">
          <CardInner>
            <CardRow label="用户">
              <TruncatedText>{meQuery.data?.username ?? "—"}</TruncatedText>
            </CardRow>
            <CardActions>
              <button
                type="button"
                className="card-action-button primary"
                onClick={() => setPanelOpen(v => !v)}
              >
                <Edit2 size={12} /><span>修改密码</span>
              </button>
            </CardActions>
          </CardInner>
        </div>
      </div>
      {panelOpen && <ChangePasswordPanel onClose={() => setPanelOpen(false)} />}
    </section>
  );
}

export function SettingsView() {
  return (
    <section className="view-stack">
      <AccountSection />
    </section>
  );
}
