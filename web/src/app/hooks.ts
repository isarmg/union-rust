/*
 * React 自定义 Hook。
 *
 * 这里集中实现实时事件流；SSE 直接写入 React Query 缓存，
 * HTTP 轮询在断线时接管，因此服务状态只有一个数据源。
 */
import { useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { realtimeApi as api } from "./realtimeApi";
import { overviewQueryKeys as queryKeys } from "../features/overview/queryKeys";
import type { ServiceStatus } from "../features/overview/types";

interface EventPayload {
  kind: string;
  generated_at: string;
  services: ServiceStatus[];
}

/**
 * 订阅后端的 Server-Sent Events 实时事件。
 *
 * SSE 可以理解成“后端主动向浏览器推消息”的轻量连接：
 * - 浏览器用 EventSource 连接 /api/events。
 * - 后端有服务状态变化时推送 status 事件。
 * - 页面收到事件后更新本地状态，用户就能看到更及时的运行状态。
 */
export function useEventStream(enabled = true) {
  const queryClient = useQueryClient();
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!enabled) {
      setConnected(false);
      setError(null);
      return;
    }
    // EventSource 不支持自定义请求头，因此先向后端申请一个 60 秒有效的短效
    // ticket，再把 ticket 放入 URL 参数。这样服务器日志里不会出现长效 session
    // token，泄露风险大幅降低。
    // 断线后手动重连（每 5 秒一次），重连时重新申请 ticket。
    let source: EventSource | null = null;
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
    let cancelled = false;
    let connecting = false;

    function scheduleReconnect(message: string) {
      setConnected(false);
      setError(message);
      void queryClient.invalidateQueries({ queryKey: queryKeys.services });
      if (!cancelled && reconnectTimer === null) {
        reconnectTimer = setTimeout(() => {
          reconnectTimer = null;
          connect();
        }, 5000);
      }
    }

    function disconnect(target: EventSource, message: string) {
      // A closed EventSource may still deliver a queued callback. It must never
      // tear down the newer connection that replaced it.
      if (source !== target) return;
      source = null;
      target.close();
      scheduleReconnect(message);
    }

    function connect() {
      if (cancelled || connecting || source !== null) return;
      connecting = true;
      api.issueSseTicket()
        .then(({ ticket }) => {
          connecting = false;
          if (cancelled) return;
          const nextSource = new EventSource(`/api/events?ticket=${encodeURIComponent(ticket)}`);
          source = nextSource;

          nextSource.addEventListener("open", () => {
            if (source !== nextSource) return;
            setConnected(true);
            setError(null);
          });

          nextSource.addEventListener("status", (event) => {
            if (source !== nextSource) return;
            try {
              const payload = JSON.parse(event.data) as EventPayload;
              if (!Array.isArray(payload.services)) throw new Error("invalid services payload");
              // Cancel the older HTTP snapshot before publishing this event. React Query's
              // cancellation is synchronous even though the cleanup promise is returned, so a
              // request that resolves later cannot overwrite the newer SSE state.
              void queryClient.cancelQueries({ queryKey: queryKeys.services, exact: true });
              queryClient.setQueryData(queryKeys.services, payload.services);
            } catch {
              // A malformed stream must not leave polling disabled with stale cache data.
              disconnect(nextSource, "实时状态解析失败，已切换为轮询");
            }
          });

          nextSource.addEventListener("error", () => {
            // SSE 断开时页面不会崩溃；组件仍可使用 React Query 的普通轮询数据作为兜底。
            disconnect(nextSource, "实时连接已中断，已切换为轮询");
          });
        })
        .catch(() => {
          connecting = false;
          if (!cancelled && source === null) scheduleReconnect("无法建立实时连接，已使用轮询");
        });
    }

    connect();

    return () => {
      cancelled = true;
      const activeSource = source;
      source = null;
      activeSource?.close();
      if (reconnectTimer !== null) clearTimeout(reconnectTimer);
    };
  }, [enabled, queryClient]);

  return { connected, error };
}
