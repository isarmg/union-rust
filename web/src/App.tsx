import { FormEvent, lazy, Suspense, useEffect, useState } from "react";
import {
  Check, Gamepad2, LayoutDashboard, Lock, LogIn, MonitorCog, Moon, Plus, Power,
  RefreshCw, Settings, Sun, Terminal, User, X,
} from "lucide-react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, ApiError } from "./api";
import { parseAgentActivationRoute } from "./agent-activation";
import { useEventStream, useMetricHistory } from "./hooks";
import { queryKeys } from "./query-keys";
import { OverviewView } from "./views/OverviewView";
import { AgentActivationPage } from "./views/AgentActivationPage";
// 其余视图按路由懒加载。首屏只需要 Overview，而 SunshineView 一个文件就 714 行；
// 全部打进同一个 bundle 意味着只看总览的用户也要下载全部四个视图的代码。
const SunshineView = lazy(() => import("./views/SunshineView").then((m) => ({ default: m.SunshineView })));
const LogsView = lazy(() => import("./views/LogsView").then((m) => ({ default: m.LogsView })));
const SettingsView = lazy(() => import("./views/SettingsView").then((m) => ({ default: m.SettingsView })));
const MonitoringView = lazy(() => import("./views/MonitoringView").then((m) => ({ default: m.MonitoringView })));
import { CardActions, CardInner, CardRow, InlineNotice, LoadingBlock } from "./components/ui";

const navItems = [
  { key: "overview", label: "总览", icon: LayoutDashboard },
  { key: "monitoring", label: "主机", icon: MonitorCog },
  { key: "sunshine", label: "Sunshine", icon: Gamepad2 },
  { key: "logs", label: "日志", icon: Terminal },
  { key: "settings", label: "设置", icon: Settings },
] as const satisfies ReadonlyArray<{
  key: string; label: string; icon: React.ComponentType<{ size?: number }>;
}>;
type ViewKey = (typeof navItems)[number]["key"];
type Theme = "light" | "dark";
const THEME_STORAGE_KEY = "unionc-theme";

function getInitialTheme(): Theme {
  try {
    const stored = window.localStorage.getItem(THEME_STORAGE_KEY);
    if (stored === "light" || stored === "dark") return stored;
  } catch { /* local storage may be unavailable */ }
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

function AuthedApp({ onLogout }: { onLogout: () => Promise<void> }) {
  const [view, setView] = useState<ViewKey>("overview");
  const [theme, setTheme] = useState<Theme>(getInitialTheme);
  const [addTrigger, setAddTrigger] = useState(0);
  const queryClient = useQueryClient();
  const eventStream = useEventStream();

  useEffect(() => {
    try { window.localStorage.setItem(THEME_STORAGE_KEY, theme); } catch { /* ignore */ }
    document.documentElement.style.colorScheme = theme;
  }, [theme]);

  const servicesQuery = useQuery({
    queryKey: queryKeys.services,
    queryFn: api.services,
    refetchInterval: eventStream.connected ? false : 10_000,
  });
  const resourcesQuery = useQuery({
    queryKey: queryKeys.systemResources,
    queryFn: api.systemResources,
    refetchInterval: 20_000,
  });
  const history = useMetricHistory(resourcesQuery.data);
  const services = servicesQuery.data ?? [];
  const unhealthy = services.filter((service) => !service.healthy);

  return (
    <div className="app-shell" data-theme={theme}>
      <aside className="sidebar">
        <nav className="nav-list" aria-label="UnionC 导航">
          {navItems.map(({ key, label, icon: Icon }) => (
            <button
              key={key}
              className={view === key ? "nav-item active" : "nav-item"}
              aria-current={view === key ? "page" : undefined}
              type="button"
              onClick={() => { setView(key); setAddTrigger(0); }}
              title={label}
            >
              <Icon size={18} /><span>{label}</span>
            </button>
          ))}
        </nav>
        <div className="sidebar-footer">
          {view === "sunshine" && (
            <button className="icon-button" type="button" title="新建实例" aria-label="新建 Sunshine 实例" onClick={() => setAddTrigger((value) => value + 1)}>
              <Plus size={18} />
            </button>
          )}
          <button className="icon-button" type="button" onClick={() => { void queryClient.invalidateQueries(); }} title="刷新全部数据" aria-label="刷新全部数据">
            <RefreshCw size={18} />
          </button>
          <div
            className="connection-pill"
            role="status"
            aria-label={eventStream.connected ? "实时连接已建立" : "实时连接已断开，正在使用轮询"}
            title={eventStream.connected ? "实时连接已建立" : "实时连接已断开，正在使用轮询"}
          >
            {eventStream.connected
              ? <Check size={14} className="conn-icon connected" />
              : <X size={14} className="conn-icon disconnected" />}
          </div>
          <button className="icon-button" type="button" onClick={() => setTheme(theme === "light" ? "dark" : "light")} title="切换主题" aria-label={`切换到${theme === "light" ? "深色" : "浅色"}主题`}>
            {theme === "light" ? <Moon size={18} /> : <Sun size={18} />}
          </button>
          <button className="icon-button" type="button" onClick={onLogout} title="退出登录" aria-label="退出登录"><Power size={18} /></button>
        </div>
      </aside>
      <main className="main">
        {eventStream.error && <InlineNotice tone="warn" text={eventStream.error} />}
        {servicesQuery.error && <InlineNotice tone="danger" text={`服务状态读取失败：${servicesQuery.error.message}`} />}
        {resourcesQuery.error && <InlineNotice tone="danger" text={`系统资源读取失败：${resourcesQuery.error.message}`} />}
        {view === "overview" && (
          <OverviewView
            services={services}
            unhealthyCount={unhealthy.length}
            resources={resourcesQuery.data}
            history={history}
            loading={servicesQuery.isLoading || resourcesQuery.isLoading}
          />
        )}
        {/* 懒加载的分块在切换视图时才请求，用 Suspense 兜住这段空窗。 */}
        <Suspense fallback={<LoadingBlock label="正在加载视图…" />}>
          {view === "monitoring" && <MonitoringView />}
          {view === "sunshine" && <SunshineView addTrigger={addTrigger} />}
          {view === "logs" && <LogsView />}
          {view === "settings" && <SettingsView />}
        </Suspense>
      </main>
    </div>
  );
}

function LoginScreen({ onLogin }: { onLogin: (username: string, password: string) => Promise<void> }) {
  const [username, setUsername] = useState("admin");
  const [password, setPassword] = useState("");
  const [error, setError] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault(); setError(""); setSubmitting(true);
    try { await onLogin(username.trim(), password); }
    catch (loginError) { setError(loginError instanceof Error ? loginError.message : "登录失败"); }
    finally { setSubmitting(false); }
  };
  return (
    <main className="app-shell login-screen">
      <form className="content-card login-card" onSubmit={submit} aria-label="登录 UnionC 管理中心">
        <CardInner>
          <CardRow label={<><span className="login-label-icon"><User /></span>账号</>} />
          <CardRow label=""><input className="login-input" aria-label="账号" value={username} onChange={(event) => setUsername(event.target.value)} autoComplete="username" autoFocus required /></CardRow>
          <CardRow label={<><span className="login-label-icon"><Lock /></span>密码</>} />
          <CardRow label=""><input className="login-input" aria-label="密码" type="password" value={password} onChange={(event) => setPassword(event.target.value)} autoComplete="current-password" required /></CardRow>
          <CardRow label="" row={5}>{error ? <span className="login-error" role="alert">{error}</span> : null}</CardRow>
          <CardActions label={<><span className="login-label-icon"><LogIn /></span>操作</>}>
            <button className="card-action-button primary" type="submit" disabled={submitting || !username.trim() || !password}>
              <span>{submitting ? "正在登录…" : "登录"}</span>
            </button>
          </CardActions>
        </CardInner>
      </form>
    </main>
  );
}

function AuthenticatedAppRoot() {
  const queryClient = useQueryClient();
  const meQuery = useQuery({ queryKey: queryKeys.auth.me, queryFn: api.authenticate, retry: false });
  useEffect(() => {
    const expire = () => { void queryClient.invalidateQueries({ queryKey: queryKeys.auth.me }); };
    window.addEventListener("unionc:auth-expired", expire);
    return () => window.removeEventListener("unionc:auth-expired", expire);
  }, [queryClient]);
  const handleLogout = async () => {
    try { await api.logout(); } catch { /* ignore */ }
    await queryClient.resetQueries({ queryKey: queryKeys.auth.me });
  };
  const handleLogin = async (username: string, password: string) => {
    const result = await api.login(username, password);
    queryClient.setQueryData(queryKeys.auth.me, { username: result.username });
  };
  if (meQuery.isPending) return <main className="app-shell login-screen"><LoadingBlock label="正在验证会话" /></main>;
  if (meQuery.isError) {
    if (meQuery.error instanceof ApiError && meQuery.error.status === 401) {
      return <LoginScreen onLogin={handleLogin} />;
    }
    return (
      <main className="app-shell login-screen">
        <section className="session-error-card" role="alert">
          <h1>无法验证会话</h1>
          <p>{meQuery.error.message}</p>
          <button className="action-button primary" type="button" onClick={() => { void meQuery.refetch(); }}>
            <RefreshCw size={16} aria-hidden="true" /><span>重试</span>
          </button>
        </section>
      </main>
    );
  }
  return <AuthedApp onLogout={handleLogout} />;
}

export function App() {
  const activationRoute = parseAgentActivationRoute(window.location.pathname);
  if (activationRoute.isActivationRoute) {
    return <AgentActivationPage requestId={activationRoute.requestId} />;
  }
  return <AuthenticatedAppRoot />;
}
