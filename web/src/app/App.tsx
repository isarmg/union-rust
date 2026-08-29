import { FormEvent, lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Check, LayoutDashboard, Lock, LogIn, Moon, PackageOpen, Plus, Power, RefreshCw,
  Settings, Sun, User, X,
} from "lucide-react";
import { QueryClient, QueryClientProvider, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ApiError,
  advanceAuthSessionGeneration,
  currentAuthSessionGeneration,
} from "../shared/api/client";
import { authApi, type AuthIdentity } from "../features/auth/api";
import { authQueryKeys } from "../features/auth/queryKeys";
import { overviewApi } from "../features/overview/api";
import { overviewQueryKeys } from "../features/overview/queryKeys";
import { useEventStream } from "./hooks";
import { OverviewView } from "../features/overview/OverviewView";
const SettingsView = lazy(() => import("../features/settings/SettingsView").then((m) => ({ default: m.SettingsView })));
import { platformApi } from "../features/platform/api";
import { platformQueryKeys } from "../features/platform/queryKeys";
import { CardActions, CardInner, CardRow, InlineNotice, LoadingBlock } from "../shared/components/ui";
import { ModuleErrorBoundary } from "./ModuleErrorBoundary";
import { useModuleRuntime } from "./useModuleRuntime";
import {
  createModuleApi,
  createPermissionChecker,
  matchModuleRoute,
  type LoadedWebModule,
} from "./moduleRuntime";

const coreNavItems = [
  { path: "/overview", label: "总览", icon: LayoutDashboard },
  { path: "/settings", label: "设置", icon: Settings },
] satisfies ReadonlyArray<{
  path: string; label: string; icon: React.ComponentType<{ size?: number }>;
}>;
type Theme = "light" | "dark";
const THEME_STORAGE_KEY = "unionc-theme";

function initialShellPath(): string {
  return window.location.pathname === "/" ? "/overview" : window.location.pathname;
}

function useShellNavigation() {
  const [pathname, setPathname] = useState(initialShellPath);
  useEffect(() => {
    const onPopState = () => setPathname(initialShellPath());
    window.addEventListener("popstate", onPopState);
    return () => window.removeEventListener("popstate", onPopState);
  }, []);
  const navigate = useCallback((path: string, options?: { replace?: boolean }) => {
    if (!path.startsWith("/") || path.startsWith("//") || path.includes("\\")
        || path.includes("?") || path.includes("#")) return;
    if (options?.replace) window.history.replaceState(null, "", path);
    else window.history.pushState(null, "", path);
    setPathname(path);
  }, []);
  return { pathname, navigate };
}

function getInitialTheme(): Theme {
  try {
    const stored = window.localStorage.getItem(THEME_STORAGE_KEY);
    if (stored === "light" || stored === "dark") return stored;
  } catch { /* local storage may be unavailable */ }
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

function AuthedApp({
  identity,
  onLogout,
  onPasswordChanged,
}: {
  identity: AuthIdentity;
  onLogout: () => Promise<void>;
  onPasswordChanged: () => void;
}) {
  const { pathname, navigate } = useShellNavigation();
  const [theme, setTheme] = useState<Theme>(getInitialTheme);
  const addSequencesRef = useRef<Record<string, number>>({});
  const [addTriggers, setAddTriggers] = useState<Record<string, number>>({});
  const queryClient = useQueryClient();
  const eventStream = useEventStream();
  const handleAddTrigger = useCallback((moduleId: string, trigger: number) => {
    setAddTriggers((current) => {
      if (current[moduleId] !== trigger) return current;
      const next = { ...current };
      delete next[moduleId];
      return next;
    });
  }, []);

  useEffect(() => {
    try { window.localStorage.setItem(THEME_STORAGE_KEY, theme); } catch { /* ignore */ }
    document.documentElement.style.colorScheme = theme;
  }, [theme]);

  const servicesQuery = useQuery({
    queryKey: overviewQueryKeys.services,
    queryFn: overviewApi.services,
    refetchInterval: eventStream.connected ? false : 10_000,
  });
  const modulesQuery = useQuery({
    queryKey: platformQueryKeys.modules,
    queryFn: platformApi.modules,
    refetchInterval: 30_000,
  });
  const runtime = useModuleRuntime(modulesQuery.data, identity.permissions ?? []);
  const hasPermission = useMemo(
    () => createPermissionChecker(identity.permissions),
    [identity.permissions],
  );
  const loadedById = useMemo(
    () => new Map(runtime.loaded.map((module) => [module.manifest.id, module] as const)),
    [runtime.loaded],
  );
  const failedById = useMemo(
    () => new Map(runtime.failed.map((module) => [module.manifest.id, module] as const)),
    [runtime.failed],
  );
  const moduleNavigation = runtime.catalog.flatMap((module) => (
    module.enabled && module.frontend
      ? module.frontend.menu
        .filter((item) => hasPermission(item.permission))
        .map((item) => ({ module, item }))
      : []
  )).sort((left, right) => left.item.order - right.item.order
    || left.module.id.localeCompare(right.module.id)
    || left.item.id.localeCompare(right.item.id));
  const matched = runtime.loaded.flatMap((module) => {
    const route = matchModuleRoute(module, pathname);
    return route ? [{ module, route }] : [];
  })[0];
  const matchedPermission = matched ? hasPermission(matched.route.route.permission) : false;
  const activeAction = matched?.module.activation.primaryActions?.find(
    (action) => action.component === matched.route.route.component
      && (!action.permission || hasPermission(action.permission)),
  );
  const actionKey = matched ? `${matched.module.manifest.id}:${matched.route.route.component}` : "";
  const services = servicesQuery.data ?? [];
  const unhealthy = services.filter((service) => !service.healthy);

  const renderModuleRoute = (module: LoadedWebModule) => {
    if (!matched || matched.module !== module) return null;
    if (!matchedPermission) {
      return <section className="module-error-card" role="alert"><h1>无权访问模块页面</h1><p>当前账号缺少 {matched.route.route.permission} 权限。</p></section>;
    }
    const Component = matched.route.component;
    return (
      <ModuleErrorBoundary moduleId={module.manifest.id} route={pathname}>
        <section className="module-view" data-union-module={module.manifest.id}>
          <Component
            api={createModuleApi(module.manifest.frontend.api_base)}
            location={{ pathname, params: matched.route.params }}
            navigate={navigate}
            hasPermission={(permission) => hasPermission(permission)}
            actionRequest={addTriggers[actionKey] ?? 0}
            onActionRequestHandled={(trigger) => handleAddTrigger(actionKey, trigger)}
          />
        </section>
      </ModuleErrorBoundary>
    );
  };

  return (
    <div className="app-shell sarmg-theme" data-sarmg-scope data-sarmg-theme={theme}>
      <aside className="sidebar">
        <nav className="nav-list" aria-label="UnionC 导航">
          {coreNavItems.slice(0, 1).map(({ path, label, icon: Icon }) => (
            <button
              key={path}
              className={pathname === path ? "nav-item active" : "nav-item"}
              aria-current={pathname === path ? "page" : undefined}
              type="button"
              onClick={() => navigate(path)}
              title={label}
            >
              <Icon size={18} /><span>{label}</span>
            </button>
          ))}
          {moduleNavigation.map(({ module, item }) => {
            const failed = failedById.get(module.id);
            const ready = loadedById.has(module.id);
            return (
              <button
                key={`${module.id}:${item.id}`}
                className={pathname === item.route ? "nav-item active" : "nav-item"}
                aria-current={pathname === item.route ? "page" : undefined}
                type="button"
                onClick={() => navigate(item.route)}
                disabled={!ready}
                title={failed ? `${module.description}（${failed.error.message}）` : module.description}
              >
                <PackageOpen size={18} /><span>{item.label}</span>
              </button>
            );
          })}
          {coreNavItems.slice(1).map(({ path, label, icon: Icon }) => (
            <button
              key={path}
              className={pathname === path ? "nav-item active" : "nav-item"}
              aria-current={pathname === path ? "page" : undefined}
              type="button"
              onClick={() => navigate(path)}
              title={label}
            >
              <Icon size={18} /><span>{label}</span>
            </button>
          ))}
        </nav>
        <div className="sidebar-footer">
          {activeAction && (
            <button
              className="icon-button"
              type="button"
              title={activeAction.label}
              aria-label={activeAction.label}
              onClick={() => {
                const next = (addSequencesRef.current[actionKey] ?? 0) + 1;
                addSequencesRef.current[actionKey] = next;
                setAddTriggers((current) => ({ ...current, [actionKey]: next }));
              }}
            >
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
        {modulesQuery.error && <InlineNotice tone="warn" text={`模块目录读取失败：${modulesQuery.error.message}`} />}
        {runtime.catalogIssues.map((issue) => <InlineNotice key={issue.moduleId} tone="warn" text={`模块 ${issue.moduleId} Manifest 无效：${issue.message}`} />)}
        {runtime.failed.map((module) => <InlineNotice key={module.manifest.id} tone="warn" text={`模块 ${module.manifest.display_name} 加载失败：${module.error.message}`} />)}
        {servicesQuery.error && <InlineNotice tone="danger" text={`服务状态读取失败：${servicesQuery.error.message}`} />}
        {pathname === "/overview" && (
          <OverviewView
            services={services}
            unhealthyCount={unhealthy.length}
            loading={servicesQuery.isLoading}
          />
        )}
        {/* 懒加载的分块在切换视图时才请求，用 Suspense 兜住这段空窗。 */}
        <Suspense fallback={<LoadingBlock label="正在加载视图…" />}>
          {matched && renderModuleRoute(matched.module)}
          {pathname === "/settings" && <SettingsView onPasswordChanged={onPasswordChanged} />}
          {!matched && pathname.startsWith("/modules/") && runtime.loading && <LoadingBlock label="正在加载模块…" />}
          {!matched && pathname !== "/overview" && pathname !== "/settings"
            && !runtime.loading && (
              <section className="module-error-card" role="alert">
                <h1>页面不可用</h1><p>路由未注册、模块未启用，或当前账号无权访问。</p>
              </section>
            )}
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
    <main className="app-shell sarmg-theme sarmg-login" data-sarmg-scope data-sarmg-theme="system">
      <form className="sarmg-card sarmg-login__card" onSubmit={submit} aria-label="登录 UnionC 管理中心">
        <CardInner>
          <CardRow label={<><span className="sarmg-login__label-icon"><User /></span>账号</>} />
          <CardRow label=""><input className="sarmg-login__input" aria-label="账号" value={username} onChange={(event) => setUsername(event.target.value)} autoComplete="username" autoFocus required /></CardRow>
          <CardRow label={<><span className="sarmg-login__label-icon"><Lock /></span>密码</>} />
          <CardRow label=""><input className="sarmg-login__input" aria-label="密码" type="password" value={password} onChange={(event) => setPassword(event.target.value)} autoComplete="current-password" required /></CardRow>
          <CardRow label="" row={5}>{error ? <span className="sarmg-login__error" role="alert">{error}</span> : null}</CardRow>
          <CardActions label={<><span className="sarmg-login__label-icon"><LogIn /></span>操作</>}>
            <button className="sarmg-card__action sarmg-action-primary" type="submit" disabled={loginBlocked || submitting || !username.trim() || !password}>
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
  if (meQuery.isPending) return <main className="app-shell sarmg-theme sarmg-login" data-sarmg-scope data-sarmg-theme="system"><LoadingBlock label="正在验证会话" /></main>;
  if (meQuery.isError) {
    if (meQuery.error instanceof ApiError && meQuery.error.status === 401) {
      return <LoginScreen onLogin={onLogin} />;
    }
    return (
      <main className="app-shell sarmg-theme sarmg-login" data-sarmg-scope data-sarmg-theme="system">
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
  return <AuthedApp identity={meQuery.data} onLogout={onLogout} onPasswordChanged={onPasswordChanged} />;
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
    const nextQueryClient = new QueryClient({
      defaultOptions: previousQueryClient.getDefaultOptions(),
    });
    sessionQueryClientRef.current = nextQueryClient;
    setSessionQueryClient(nextQueryClient);
    // Clear after React has detached the old observers. Clearing while they are still mounted can
    // make React Query immediately recreate/refetch the private queries we are trying to discard.
    queueMicrotask(() => previousQueryClient.clear());
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
    const identity = await authApi.authenticate();
    if (loginQueryClient !== sessionQueryClientRef.current) {
      throw new Error("会话状态已改变，请重新登录");
    }
    if (identity.username !== result.username) throw new Error("登录身份与会话身份不一致");
    sessionGenerationRef.current = advanceAuthSessionGeneration();
    loginQueryClient.setQueryData(authQueryKeys.me, identity);
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
  return <AuthenticatedAppRoot />;
}
