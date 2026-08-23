import { Edit2, ExternalLink, Trash2 } from "lucide-react";
import { InlineEditableField } from "../../../shared/components/InlineEditableField";
import {
  CardActions,
  CardInner,
  CardRow,
  StatusLed,
} from "../../../shared/components/ui";
import { isOptimisticSunshineHost } from "../data";
import type { SunshineHostInfo, SunshineHostPatchRequest } from "../types";

const RE_IPV4 = /^((25[0-5]|2[0-4]\d|1\d{2}|[1-9]\d|\d)\.){3}(25[0-5]|2[0-4]\d|1\d{2}|[1-9]\d|\d)$/;
const RE_DOMAIN = /^(?!-)[A-Za-z0-9-]{1,63}(?<!-)(\.[A-Za-z0-9-]{1,63}(?<!-))*\.?$/;

function isValidIpv6(value: string): boolean {
  const inner = value.startsWith("[") && value.endsWith("]") ? value.slice(1, -1) : value;
  if (!inner.includes(":")) return false;
  try {
    return new URL(`http://[${inner}]/`).hostname.startsWith("[");
  } catch {
    return false;
  }
}

function isValidHost(value: string): boolean {
  return RE_IPV4.test(value) || isValidIpv6(value) || RE_DOMAIN.test(value);
}

export { InlineEditableField as InlineHostField };

export function HostCard({ host, selected, updating, onOpen, onDelete, onInlineUpdate }: {
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
          <InlineEditableField
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
            <InlineEditableField
              label="地址"
              value={host.host}
              validate={(value) => isValidHost(value) ? null : "请输入有效的 IPv4、IPv6 或域名"}
              onSave={(address) => onInlineUpdate({ host: address })}
              maxLength={253}
              disabled={controlsDisabled}
            />
            <span className="sunshine-inline-separator">:</span>
            <InlineEditableField
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
          <InlineEditableField
            label="账号"
            value={host.username}
            validate={(value) => value && value.length <= 256 ? null : "账号必须为 1–256 个字符"}
            onSave={(username) => onInlineUpdate({ username })}
            maxLength={256}
            disabled={controlsDisabled}
          />
        </CardRow>
        <CardRow label="密码">
          <InlineEditableField
            label="密码"
            value=""
            displayValue={host.password_set ? "已设置" : "未设置"}
            inputType="password"
            validate={(value) => value.length <= 4096 ? null : "密码不能超过 4096 个字符"}
            onSave={(password) => onInlineUpdate({ password })}
            normalize={(value) => value}
            cancelEmpty
            maxLength={4096}
            disabled={controlsDisabled}
          />
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
          <button
            type="button"
            className="card-action-button"
            disabled={controlsDisabled}
            title={controlsDisabled ? "正在保存主机，请稍候" : "仅开发模式允许关闭证书验证；生产模式会拒绝此操作"}
            onClick={() => {
              if (!host.verify_tls || window.confirm("仅开发模式允许关闭 TLS 证书验证；生产模式会拒绝。仍要尝试吗？")) {
                void onInlineUpdate({ verify_tls: !host.verify_tls });
              }
            }}
          >
            {host.verify_tls ? "验证证书" : "允许自签名"}
          </button>
        </CardRow>
        <CardActions>
          <button type="button" className="card-action-button" disabled={controlsDisabled} onClick={onOpen}>
            <Edit2 size={12} /><span>{selected ? "收起管理" : "管理"}</span>
          </button>
          <button type="button" className="card-action-button danger" disabled={controlsDisabled} onClick={onDelete}>
            <Trash2 size={12} /><span>删除</span>
          </button>
          <a
            href={controlsDisabled ? undefined : host.web_url}
            target="_blank"
            rel="noopener noreferrer"
            className="card-action-button primary"
            aria-disabled={controlsDisabled}
            tabIndex={controlsDisabled ? -1 : undefined}
            onClick={(event) => { if (controlsDisabled) event.preventDefault(); }}
          >
            <ExternalLink size={12} /><span>打开</span>
          </a>
        </CardActions>
      </CardInner>
    </article>
  );
}
