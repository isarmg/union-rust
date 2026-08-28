function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isOptionalBoolean(value) {
  return value === null || value === undefined || typeof value === "boolean";
}

function isSunshineHost(value) {
  return isRecord(value)
    && typeof value.id === "string"
    && typeof value.name === "string"
    && typeof value.host === "string"
    && Number.isInteger(value.web_port)
    && typeof value.username === "string"
    && typeof value.verify_tls === "boolean"
    && typeof value.web_url === "string"
    && typeof value.probe_status === "string"
    && isOptionalBoolean(value.reachable)
    && isOptionalBoolean(value.connected)
    && (value.connection_error === null
      || value.connection_error === undefined
      || typeof value.connection_error === "string");
}

export async function loadSunshineHosts(api) {
  const response = await api.request("/hosts");
  if (!Array.isArray(response) || !response.every(isSunshineHost)) {
    throw new Error("Sunshine 主机列表响应格式无效");
  }
  return response;
}

function errorMessage(error) {
  return error instanceof Error ? error.message : "读取模块数据失败";
}

function connectionLabel(host) {
  if (host.probe_status !== "complete") return "等待探测";
  if (host.reachable === false) return "不可访问";
  if (host.connected === true) return "已连接";
  if (host.reachable === true) return "可访问";
  return "状态未知";
}

function connectionTone(host) {
  if (host.probe_status !== "complete" || host.reachable == null) return "pending";
  if (host.reachable === false || host.connected === false) return "error";
  return "ready";
}

const entry = {
  pluginApiVersion: "1.0.0",
  moduleId: "sunshine",
  version: "0.5.0",
  activate(host) {
    const {
      createElement: h,
      useCallback,
      useEffect,
      useState,
    } = host.react;

    function SunshineHostList({ diagnostics = false }) {
      const [hosts, setHosts] = useState([]);
      const [loading, setLoading] = useState(true);
      const [error, setError] = useState(null);

      const refresh = useCallback(async () => {
        setLoading(true);
        setError(null);
        try {
          setHosts(await loadSunshineHosts(host.api));
        } catch (requestError) {
          setError(errorMessage(requestError));
        } finally {
          setLoading(false);
        }
      }, []);

      useEffect(() => {
        void refresh();
      }, [refresh]);

      const connected = hosts.filter((item) => item.connected === true).length;
      const title = diagnostics ? "Sunshine 连接诊断" : "Sunshine 主机";
      const description = diagnostics
        ? "查看各主机的最近探测结果和连接错误。"
        : "查看由 Sunshine 模块管理的主机与连接状态。";

      return h(
        "section",
        { className: "union-module sunshine-module", "aria-labelledby": "sunshine-module-title" },
        h(
          "header",
          { className: "union-module__header" },
          h("div", null,
            h("p", { className: "union-module__eyebrow" }, "业务模块"),
            h("h1", { id: "sunshine-module-title" }, title),
            h("p", { className: "union-module__description" }, description)),
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
          { className: "union-module__summary", "aria-label": "Sunshine 主机摘要" },
          h("div", null, h("strong", null, String(hosts.length)), h("span", null, "主机")),
          h("div", null, h("strong", null, String(connected)), h("span", null, "已连接")),
          h("code", null, host.api.basePath),
        ),
        loading && hosts.length === 0
          ? h("p", { className: "union-module__state", role: "status" }, "正在加载主机…")
          : null,
        error
          ? h(
            "div",
            { className: "union-module__state union-module__state--error", role: "alert" },
            h("p", null, error),
            h("button", { type: "button", onClick: () => { void refresh(); } }, "重试"),
          )
          : null,
        !loading && !error && hosts.length === 0
          ? h("p", { className: "union-module__state" }, "尚未配置 Sunshine 主机。")
          : null,
        hosts.length > 0
          ? h(
            "div",
            { className: "union-module__grid" },
            hosts.map((item) => h(
              "article",
              { className: "union-module__card", key: item.id },
              h(
                "div",
                { className: "union-module__card-heading" },
                h("div", null,
                  h("h2", null, item.name || item.host),
                  h("p", null, item.host + ":" + item.web_port)),
                h(
                  "span",
                  { className: "union-module__badge union-module__badge--" + connectionTone(item) },
                  connectionLabel(item),
                ),
              ),
              h(
                "dl",
                { className: "union-module__details" },
                h("div", null, h("dt", null, "用户名"), h("dd", null, item.username || "—")),
                h("div", null, h("dt", null, "TLS 校验"), h("dd", null, item.verify_tls ? "启用" : "关闭")),
                h("div", null, h("dt", null, "管理地址"), h("dd", null, h("code", null, item.web_url))),
              ),
              item.connection_error
                ? h("p", { className: "union-module__diagnostic" }, item.connection_error)
                : diagnostics
                  ? h("p", { className: "union-module__diagnostic union-module__diagnostic--ok" }, "最近探测未报告错误。")
                  : null,
            )),
          )
          : null,
      );
    }

    function SunshineView() {
      return h(SunshineHostList);
    }

    function SunshineLogsView() {
      return h(SunshineHostList, { diagnostics: true });
    }

    return {
      components: {
        SunshineView,
        SunshineLogsView,
      },
    };
  },
};

export default entry;
