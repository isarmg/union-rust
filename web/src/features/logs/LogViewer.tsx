import type { LogsResponse } from "./types";

export function LogViewer({
  logs,
  loading,
}: {
  logs: LogsResponse | undefined;
  loading: boolean;
}) {
  return (
    <div className="log-viewer">
      <div className="log-toolbar">
        <span>{logs?.path ?? "等待日志文件"}</span>
        <span>{logs?.lines.length ?? 0} 行</span>
      </div>
      <pre>
        {loading
          ? "loading..."
          : logs?.lines.length
            ? logs.lines.join("\n")
            : "暂无日志"}
      </pre>
    </div>
  );
}
