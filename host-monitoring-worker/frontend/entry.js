function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isHostSummary(value) {
  return isRecord(value)
    && typeof value.id === "string"
    && typeof value.name === "string"
    && typeof value.os === "string"
    && typeof value.arch === "string"
    && typeof value.agent_version === "string"
    && typeof value.status === "string"
    && typeof value.last_seen_at === "string";
}

function isAgentInstance(value) {
  return isRecord(value)
    && typeof value.request_id === "string"
    && typeof value.instance_id === "string"
    && typeof value.display_name === "string"
    && typeof value.status === "string"
    && typeof value.expires_at === "string"
    && typeof value.created_at === "string";
}

const CANONICAL_UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

function isPairingSummary(value) {
  return isRecord(value)
    && typeof value.request_id === "string"
    && typeof value.os === "string"
    && typeof value.arch === "string"
    && typeof value.agent_version === "string"
    && ["waiting", "expired", "denied", "active"].includes(value.status)
    && typeof value.expires_at === "string";
}

export function activationRequestId(location) {
  const requestId = location?.params?.requestId;
  return typeof requestId === "string" && CANONICAL_UUID.test(requestId) ? requestId : null;
}

export function activationCodeForSubmission(value) {
  return value.trim();
}

export async function loadPairing(api, requestId) {
  const value = await api.request(`/agent/v2/pairing-requests/${encodeURIComponent(requestId)}`);
  if (!isPairingSummary(value) || value.request_id !== requestId) {
    throw new Error("Agent 配对响应格式无效");
  }
  return value;
}

export async function activatePairing(api, requestId, activationCode) {
  const value = await api.request("/agent/v2/activate-admin", {
    method: "POST",
    body: JSON.stringify({ request_id: requestId, activation_code: activationCode }),
    suppressAuthExpired: true,
  });
  if (!isRecord(value) || typeof value.instance_id !== "string" || value.status !== "active") {
    throw new Error("Agent 激活响应格式无效");
  }
  return value;
}

export async function loadHostOverview(api) {
  const [hostResponse, instanceResponse] = await Promise.all([
    api.request("/hosts"),
    api.request("/agent-instances"),
  ]);
  if (!isRecord(hostResponse)
      || !Array.isArray(hostResponse.hosts)
      || !hostResponse.hosts.every(isHostSummary)
      || typeof hostResponse.total !== "number"
      || !Array.isArray(instanceResponse)
      || !instanceResponse.every(isAgentInstance)) {
    throw new Error("主机模块概览响应格式无效");
  }
  return {
    hosts: hostResponse.hosts,
    instances: instanceResponse,
    total: hostResponse.total,
  };
}

function errorMessage(error) {
  return error instanceof Error ? error.message : "读取模块数据失败";
}

function formatTime(value) {
  const timestamp = Date.parse(value);
  if (Number.isNaN(timestamp)) return value;
  return new Intl.DateTimeFormat("zh-CN", {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(timestamp));
}

function statusTone(status) {
  const normalized = status.toLowerCase();
  if (normalized === "active" || normalized === "online" || normalized === "healthy") return "ready";
  if (normalized === "pending" || normalized === "waiting") return "pending";
  return "error";
}

const entry = {
  pluginApiVersion: "1.0.0",
  moduleId: "host-monitoring",
  version: "0.5.0",
  activate(host) {
    const {
      createElement: h,
      useCallback,
      useEffect,
      useState,
    } = host.react;

    function HostMonitoringView({ api }) {
      const [overview, setOverview] = useState({ hosts: [], instances: [], total: 0 });
      const [loading, setLoading] = useState(true);
      const [error, setError] = useState(null);

      const refresh = useCallback(async () => {
        setLoading(true);
        setError(null);
        try {
          setOverview(await loadHostOverview(api));
        } catch (requestError) {
          setError(errorMessage(requestError));
        } finally {
          setLoading(false);
        }
      }, []);

      useEffect(() => {
        void refresh();
      }, [refresh]);

      return h(
        "section",
        { className: "union-module host-monitoring-module", "aria-labelledby": "host-module-title" },
        h(
          "header",
          { className: "union-module__header" },
          h("div", null,
            h("p", { className: "union-module__eyebrow" }, "业务模块"),
            h("h1", { id: "host-module-title" }, "主机与实例"),
            h("p", { className: "union-module__description" }, "查看受管主机状态和等待激活的实例。")),
          h(
            "button",
            {
              type: "button",
              className: "union-module__refresh",
              disabled: loading,
              onClick: () => { void refresh(); },
            },
            loading ? "刷新中…" : "刷新",
          ),
        ),
        h(
          "div",
          { className: "union-module__summary", "aria-label": "主机模块摘要" },
          h("div", null, h("strong", null, String(overview.total)), h("span", null, "受管主机")),
          h("div", null, h("strong", null, String(overview.instances.length)), h("span", null, "实例请求")),
        h("code", null, api.basePath),
        ),
        loading && overview.hosts.length === 0 && overview.instances.length === 0
          ? h("p", { className: "union-module__state", role: "status" }, "正在加载主机概览…")
          : null,
        error
          ? h(
            "div",
            { className: "union-module__state union-module__state--error", role: "alert" },
            h("p", null, error),
            h("button", { type: "button", onClick: () => { void refresh(); } }, "重试"),
          )
          : null,
        !error
          ? h(
            "div",
            { className: "union-module__columns" },
            h(
              "section",
              { className: "union-module__panel", "aria-labelledby": "managed-hosts-title" },
              h("div", { className: "union-module__panel-heading" },
                h("h2", { id: "managed-hosts-title" }, "受管主机"),
                h("span", null, String(overview.hosts.length))),
              overview.hosts.length === 0 && !loading
                ? h("p", { className: "union-module__empty" }, "尚无已激活主机。")
                : h("div", { className: "union-module__list" }, overview.hosts.map((item) => h(
                  "article",
                  { className: "union-module__list-item", key: item.id },
                  h("div", { className: "union-module__item-heading" },
                    h("div", null, h("h3", null, item.name), h("p", null, item.os + " · " + item.arch)),
                    h("span", { className: "union-module__badge union-module__badge--" + statusTone(item.status) }, item.status)),
                  h("dl", { className: "union-module__details" },
                    h("div", null, h("dt", null, "Agent"), h("dd", null, item.agent_version)),
                    h("div", null, h("dt", null, "最后在线"), h("dd", null, formatTime(item.last_seen_at)))),
                ))),
            ),
            h(
              "section",
              { className: "union-module__panel", "aria-labelledby": "agent-instances-title" },
              h("div", { className: "union-module__panel-heading" },
                h("h2", { id: "agent-instances-title" }, "实例请求"),
                h("span", null, String(overview.instances.length))),
              overview.instances.length === 0 && !loading
                ? h("p", { className: "union-module__empty" }, "当前没有等待处理的实例请求。")
                : h("div", { className: "union-module__list" }, overview.instances.map((item) => h(
                  "article",
                  { className: "union-module__list-item", key: item.request_id },
                  h("div", { className: "union-module__item-heading" },
                    h("div", null, h("h3", null, item.display_name), h("p", null, item.instance_id)),
                    h("span", { className: "union-module__badge union-module__badge--" + statusTone(item.status) }, item.status)),
                  h("dl", { className: "union-module__details" },
                    h("div", null, h("dt", null, "创建时间"), h("dd", null, formatTime(item.created_at))),
                    h("div", null, h("dt", null, "过期时间"), h("dd", null, formatTime(item.expires_at)))),
                ))),
            ),
          )
          : null,
      );
    }

    const pairingStatusLabel = {
      waiting: "等待激活",
      expired: "已过期",
      denied: "已拒绝",
      active: "已激活",
    };

    function HostActivationView({ api, location }) {
      const requestId = activationRequestId(location);
      const [pairing, setPairing] = useState(null);
      const [activation, setActivation] = useState(null);
      const [activationCode, setActivationCode] = useState("");
      const [loading, setLoading] = useState(Boolean(requestId));
      const [submitting, setSubmitting] = useState(false);
      const [error, setError] = useState(null);

      useEffect(() => {
        let current = true;
        setPairing(null);
        setActivation(null);
        setActivationCode("");
        setError(null);
        if (!requestId) {
          setLoading(false);
          return () => { current = false; };
        }
        setLoading(true);
        void loadPairing(api, requestId)
          .then((value) => { if (current) setPairing(value); })
          .catch((requestError) => { if (current) setError(errorMessage(requestError)); })
          .finally(() => { if (current) setLoading(false); });
        return () => { current = false; };
      }, [api, requestId]);

      const submit = async (event) => {
        event.preventDefault();
        const code = activationCodeForSubmission(activationCode);
        if (!requestId || !code || submitting) return;
        setSubmitting(true);
        setError(null);
        try {
          const result = await activatePairing(api, requestId, code);
          setActivationCode("");
          setActivation(result);
        } catch (requestError) {
          setError(errorMessage(requestError));
        } finally {
          setSubmitting(false);
        }
      };

      if (activation) {
        return h(
          "section",
          { className: "union-module host-monitoring-module host-activation", "aria-labelledby": "host-activation-title" },
          h("div", { className: "host-activation__card" },
            h("p", { className: "union-module__eyebrow" }, "主机模块"),
            h("h1", { id: "host-activation-title" }, "Agent 激活成功"),
            h("p", null, "此设备已与 Union 配对，可以关闭这个浏览器窗口。"),
            h("dl", { className: "union-module__details" },
              h("div", null, h("dt", null, "实例 ID"), h("dd", null, activation.instance_id)),
              h("div", null, h("dt", null, "状态"), h("dd", null, "已激活")))),
        );
      }

      const canActivate = pairing?.status === "waiting";
      return h(
        "section",
        { className: "union-module host-monitoring-module host-activation", "aria-labelledby": "host-activation-title" },
        h("div", { className: "host-activation__card" },
          h("p", { className: "union-module__eyebrow" }, "主机模块"),
          h("h1", { id: "host-activation-title" }, "激活 Union Agent"),
          h("p", { className: "union-module__description" }, "确认设备信息，并输入管理中心生成的一次性激活码。"),
          !requestId
            ? h("div", { className: "union-module__state union-module__state--error", role: "alert" }, "激活链接无效或不完整。")
            : null,
          loading ? h("p", { className: "union-module__state", role: "status" }, "正在读取 Agent 配对信息…") : null,
          error ? h("div", { className: "union-module__state union-module__state--error", role: "alert" }, error) : null,
          pairing
            ? h("dl", { className: "host-activation__summary", "aria-label": "Agent 配对摘要" },
              h("div", null, h("dt", null, "系统"), h("dd", null, [pairing.os, pairing.arch].filter(Boolean).join(" · ") || "-")),
              h("div", null, h("dt", null, "Agent"), h("dd", null, pairing.agent_version || "-")),
              h("div", null, h("dt", null, "状态"), h("dd", null, pairingStatusLabel[pairing.status])),
              h("div", null, h("dt", null, "到期时间"), h("dd", null, formatTime(pairing.expires_at))),
              h("div", null, h("dt", null, "配对请求"), h("dd", null, requestId)))
            : null,
          pairing && !canActivate
            ? h("div", { className: "union-module__state union-module__state--error", role: "alert" }, `此配对请求${pairingStatusLabel[pairing.status]}，不能再次激活。`)
            : null,
          pairing && canActivate
            ? h("form", { className: "host-activation__form", onSubmit: submit },
              h("label", { htmlFor: "host-activation-code" },
                h("span", null, "一次性激活码"),
                h("input", {
                  id: "host-activation-code",
                  value: activationCode,
                  onChange: (event) => { setActivationCode(event.target.value); setError(null); },
                  autoComplete: "one-time-code",
                  autoCapitalize: "none",
                  spellCheck: false,
                  maxLength: 128,
                  autoFocus: true,
                  required: true,
                })),
              h("button", {
                type: "submit",
                className: "union-module__refresh",
                disabled: !activationCodeForSubmission(activationCode) || submitting,
              }, submitting ? "正在激活…" : "确认激活"))
            : null),
      );
    }

    return { components: { HostMonitoringView, HostActivationView } };
  },
};

export default entry;
