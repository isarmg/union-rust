/*
 * React 应用入口。
 *
 * 浏览器先加载 index.html，index.html 里有一个 id="root" 的空 div。
 * 这个文件负责把 React 组件树挂载到 root 上，让页面真正显示出来。
 */
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { App } from "./app/App";
import { ErrorBoundary } from "./shared/components/ErrorBoundary";
import "./app/styles.css";

// React Query 负责缓存和刷新后端数据。
// 这里集中设置默认策略，避免每个 useQuery 都重复写相同配置。
const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // 管理后台的数据以轮询和手动刷新为主。
      // 关闭 refetchOnWindowFocus 可以避免切回浏览器窗口时突然触发整页请求。
      refetchOnWindowFocus: false,
      // 请求失败后自动重试 1 次，能处理偶发网络抖动，但不会无限重试。
      retry: 1,
      // 5 秒内的数据认为还比较新，短时间切换页面时不必立刻重新请求。
      staleTime: 5_000
    }
  }
});

// StrictMode 是 React 的开发辅助模式，会帮助发现副作用和生命周期问题。
createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <ErrorBoundary><App /></ErrorBoundary>
    </QueryClientProvider>
  </StrictMode>
);
