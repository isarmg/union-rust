import { FormEvent, lazy, Suspense, useCallback, useEffect, useRef, useState } from "react";
import {
  Check, Gamepad2, LayoutDashboard, Lock, LogIn, MonitorCog, Moon, Plus, Power,
  RefreshCw, Settings, Sun, Terminal, User, X,
} from "lucide-react";
import { QueryClient, QueryClientProvider, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ApiError,
  advanceAuthSessionGeneration,
  currentAuthSessionGeneration,
} from "../shared/api/client";
import { authApi } from "../features/auth/api";
import { authQueryKeys } from "../features/auth/queryKeys";
import { overviewApi } from "../features/overview/api";
import { overviewQueryKeys } from "../features/overview/queryKeys";
import { parseAgentActivationRoute } from "../features/agent-activation/route";
import { useEventStream, useMetricHistory } from "./hooks";
import { OverviewView } from "../features/overview/OverviewView";
import { AgentActivationPage } from "../features/agent-activation/AgentActivationPage";
// 其余功能按视图懒加载；只看总览的用户无需下载 Sunshine、监控、日志和设置代码。
const SunshineView = lazy(() => import("../features/sunshine/SunshineView").then((m) => ({ default: m.SunshineView })));
const LogsView = lazy(() => import("../features/logs/LogsView").then((m) => ({ default: m.LogsView })));
const SettingsView = lazy(() => import("../features/settings/SettingsView").then((m) => ({ default: m.SettingsView })));
const MonitoringView = lazy(() => import("../features/monitoring/MonitoringView").then((m) => ({ default: m.MonitoringView })));
import { CardActions, CardInner, CardRow, InlineNotice, LoadingBlock } from "../shared/components/ui";

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

function AuthedApp({
  onLogout,
  onPasswordChanged,
}: {
  onLogout: () => Promise<void>;
  onPasswordChanged: () => void;
}) {
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
    queryKey: overviewQueryKeys.services,
    queryFn: overviewApi.services,
    refetchInterval: eventStream.connected ? false : 10_000,
  });
  const resourcesQuery = useQuery({
    queryKey: overviewQueryKeys.systemResources,
    queryFn: overviewApi.systemResources,
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
          {view === "settings" && <SettingsView onPasswordChanged={onPasswordChanged} />}
        </Suspense>
      </main>
    </div>
  );
}

function LoginScreen({
  onLogin,
  loginBlocked = false,
}: {
  onLogin: (username: string, password: string) => Promise<void>;
  loginBlocked?: boolean;
}) {
  const [username, setUsername] = useState("admin");
  const [password, setPassword] = useState("");
  const [error, setError] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (loginBlocked) return;
    setError(""); setSubmitting(true);
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
            <button className="card-action-button primary" type="submit" disabled={loginBlocked || submitting || !username.trim() || !password}>
              <span>{loginBlocked ? "正在退出…" : submitting ? "正在登录…" : "登录"}</span>
            </button>
          </CardActions>
        </CardInner>
      </form>
    </main>
  );
}

function SessionBoundary({
  onLogin,
  onLogout,
  onPasswordChanged,
}: {
  onLogin: (username: string, password: string) => Promise<void>;
  onLogout: () => Promise<void>;
  onPasswordChanged: () => void;
}) {
  const meQuery = useQuery({ queryKey: authQueryKeys.me, queryFn: authApi.authenticate, retry: false });
  if (meQuery.isPending) return <main className="app-shell login-screen"><LoadingBlock label="正在验证会话" /></main>;
  if (meQuery.isError) {
    if (meQuery.error instanceof ApiError && meQuery.error.status === 401) {
      return <LoginScreen onLogin={onLogin} />;
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
  return <AuthedApp onLogout={onLogout} onPasswordChanged={onPasswordChanged} />;
}

function AuthenticatedAppRoot() {
  const parentQueryClient = useQueryClient();
  const [sessionQueryClient, setSessionQueryClient] = useState(() => new QueryClient({
    defaultOptions: parentQueryClient.getDefaultOptions(),
  }));
  const sessionQueryClientRef = useRef(sessionQueryClient);
  const sessionGenerationRef = useRef(currentAuthSessionGeneration());
  const [signedOut, setSignedOut] = useState(false);
  const [logoutPending, setLogoutPending] = useState(false);
  const logoutPendingRef = useRef(false);

  const replaceSessionQueryClient = useCallback(() => {
    const previousQueryClient = sessionQueryClientRef.current;
    sessionGenerationRef.current = advanceAuthSessionGeneration();
    previousQueryClient.clear();
    const nextQueryClient = new QueryClient({
      defaultOptions: previousQueryClient.getDefaultOptions(),
    });
    sessionQueryClientRef.current = nextQueryClient;
    setSessionQueryClient(nextQueryClient);
  }, []);

  useEffect(() => {
    const expire = (event: Event) => {
      const expiredGeneration = event instanceof CustomEvent ? event.detail : undefined;
      if (typeof expiredGeneration === "number"
        && expiredGeneration !== sessionGenerationRef.current) return;
      replaceSessionQueryClient();
      setSignedOut(true);
    };
    window.addEventListener("unionc:auth-expired", expire);
    return () => window.removeEventListener("unionc:auth-expired", expire);
  }, [replaceSessionQueryClient]);

  const handleLogout = async () => {
    if (logoutPendingRef.current) return;
    logoutPendingRef.current = true;
    setLogoutPending(true);
    // 先立即切断当前页面与全部私有缓存的联系，再尽力通知服务器注销会话。
    replaceSessionQueryClient();
    setSignedOut(true);
    try { await authApi.logout(); } catch { /* ignore */ }
    finally {
      logoutPendingRef.current = false;
      setLogoutPending(false);
    }
  };
  const handleLogin = async (username: string, password: string) => {
    if (logoutPendingRef.current) throw new Error("正在退出登录，请稍候");
    const loginQueryClient = sessionQueryClientRef.current;
    const result = await authApi.login(username, password);
    if (loginQueryClient !== sessionQueryClientRef.current) {
      throw new Error("会话状态已改变，请重新登录");
    }
    sessionGenerationRef.current = advanceAuthSessionGeneration();
    loginQueryClient.setQueryData(authQueryKeys.me, { username: result.username });
    setSignedOut(false);
  };
  const renderedSessionGeneration = sessionGenerationRef.current;
  const handlePasswordChanged = useCallback(() => {
    // A completed mutation can outlive the QueryClient and component that started it. Ignore its
    // callback after logout/login has already advanced to another browser session generation.
    if (sessionGenerationRef.current !== renderedSessionGeneration) return;
    replaceSessionQueryClient();
    setSignedOut(true);
  }, [renderedSessionGeneration, replaceSessionQueryClient]);

  return (
    <QueryClientProvider client={sessionQueryClient}>
      {signedOut
        ? <LoginScreen onLogin={handleLogin} loginBlocked={logoutPending} />
        : (
          <SessionBoundary
            onLogin={handleLogin}
            onLogout={handleLogout}
            onPasswordChanged={handlePasswordChanged}
          />
        )}
    </QueryClientProvider>
  );
}

export function App() {
  const activationRoute = parseAgentActivationRoute(window.location.pathname);
  if (activationRoute.isActivationRoute) {
    return <AgentActivationPage requestId={activationRoute.requestId} />;
  }
  return <AuthenticatedAppRoot />;
}
