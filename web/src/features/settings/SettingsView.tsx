import { useState } from "react";
import { KeyRound, Loader2, Save } from "lucide-react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { authApi as api } from "../auth/api";
import { authQueryKeys as queryKeys } from "../auth/queryKeys";
import { CardActions, CardInner, CardRow, InlineNotice, SectionHeader, TruncatedText } from "../../shared/components/ui";
import { removeMutationFromCache } from "../../shared/lib/mutations";

const changePasswordMutationKey = ["settings-change-password"] as const;

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

export function SettingsView({ onPasswordChanged }: { onPasswordChanged: () => void }) {
  return (
    <section className="view-stack">
      <AccountSection onPasswordChanged={onPasswordChanged} />
    </section>
  );
}
