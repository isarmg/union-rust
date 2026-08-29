// @vitest-environment jsdom

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { authApi as api } from "../features/auth/api";
import { authQueryKeys } from "../features/auth/queryKeys";
import { overviewApi } from "../features/overview/api";
import { overviewQueryKeys } from "../features/overview/queryKeys";
import type { ServiceStatus } from "../features/overview/types";
import { ApiError, request } from "../shared/api/client";
import { realtimeApi } from "./realtimeApi";
import { platformApi } from "../features/platform/api";
import type { PlatformModule } from "../features/platform/types";
import type { WebModuleComponentProps } from "../platform-sdk/web";
import { moduleRuntimeEnvironment } from "./moduleRuntime";

const platformModules: PlatformModule[] = [
  {
    manifest_version: 2,
    id: "inventory",
    display_name: "Inventory",
    description: "Neutral inventory fixture",
    version: "0.1.0",
    compatibility: {
      core: ">=0.6.0, <0.7.0",
      platform_api: "^1.0.0",
      plugin_api: "^2.0.0",
    },
    dependencies: [],
    permissions: [{ id: "inventory.items.read", description: "Read inventory" }],
    frontend: {
      entry: "frontend/remoteEntry.js",
      styles: [],
      components: ["InventoryView"],
      api_base: "/api/modules/inventory",
      routes: [{
        path: "/modules/inventory",
        component: "InventoryView",
        permission: "inventory.items.read",
      }],
      menu: [{
        id: "overview",
        label: "Inventory",
        route: "/modules/inventory",
        permission: "inventory.items.read",
        order: 20,
      }],
    },
    enabled: true,
    lifecycle_state: "available",
    health_message: "worker ready",
    pid: 101,
    restart_count: 0,
    checked_at: null,
    resolved_frontend: {
      entry: "/modules/inventory/assets/frontend/remoteEntry.js",
      styles: [],
    },
  },
  {
    manifest_version: 2,
    id: "reports",
    display_name: "Reports",
    description: "Neutral reports fixture",
    version: "0.1.0",
    compatibility: {
      core: ">=0.6.0, <0.7.0",
      platform_api: "^1.0.0",
      plugin_api: "^2.0.0",
    },
    dependencies: [],
    permissions: [{ id: "reports.read", description: "Read reports" }],
    frontend: {
      entry: "frontend/remoteEntry.js",
      styles: [],
      components: ["ReportsView"],
      api_base: "/api/modules/reports",
      routes: [
        { path: "/modules/reports", component: "ReportsView", permission: "reports.read" },
      ],
      menu: [
        { id: "overview", label: "Reports", route: "/modules/reports", permission: "reports.read", order: 10 },
      ],
    },
    enabled: true,
    lifecycle_state: "available",
    health_message: "worker ready",
    pid: 102,
    restart_count: 0,
    checked_at: null,
    resolved_frontend: {
      entry: "/modules/reports/assets/frontend/remoteEntry.js",
      styles: [],
    },
  },
];

const invalidRemoteModule = {
  manifest_version: 2,
  id: "unsafe-entry",
  display_name: "Unsafe entry",
  description: "Invalid cross-origin fixture",
  version: "0.1.0",
  compatibility: { core: ">=0.6.0, <0.7.0", platform_api: "^1.0.0", plugin_api: "^2.0.0" },
  dependencies: [],
  permissions: [],
  frontend: {
    entry: "https://outside.example/remoteEntry.js",
    styles: [],
    components: ["UnsafeView"],
    api_base: "/api/modules/unsafe-entry",
    routes: [{ path: "/modules/unsafe-entry", component: "UnsafeView", permission: null }],
    menu: [{
      id: "overview", label: "Unsafe", route: "/modules/unsafe-entry", permission: null, order: 30,
    }],
  },
  enabled: true,
  lifecycle_state: "available",
  health_message: "gateway-v1 ready",
  pid: 103,
  restart_count: 2,
  checked_at: "2026-08-27T00:00:00Z",
  resolved_frontend: {
    entry: "/modules/unsafe-entry/assets/https://outside.example/remoteEntry.js",
    styles: [],
  },
} as const;

function NeutralInventoryView({
  actionRequest,
  onActionRequestHandled,
}: WebModuleComponentProps) {
  const [handled, setHandled] = useState(0);
  useEffect(() => {
    if (!actionRequest) return;
    setHandled(actionRequest);
    onActionRequestHandled(actionRequest);
  }, [actionRequest, onActionRequestHandled]);
  return <section><h1>Inventory fixture</h1><output>Handled action {handled}</output></section>;
}

function renderApp() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const rendered = render(
    <QueryClientProvider client={queryClient}>
      <App />
    </QueryClientProvider>,
  );
  return { ...rendered, queryClient };
}

function mockAuthenticatedApp() {
  vi.spyOn(api, "authenticate").mockResolvedValue({ username: "admin", permissions: ["*"] });
  const services = vi.spyOn(overviewApi, "services").mockReturnValue(new Promise(() => {}));
  vi.spyOn(realtimeApi, "issueSseTicket").mockReturnValue(new Promise(() => {}));
  vi.spyOn(platformApi, "modules").mockResolvedValue(platformModules);
  vi.spyOn(platformApi, "moduleConfiguration").mockImplementation(async (module) => ({
    module,
    schema_version: 1,
    schema: { type: "object", properties: {} },
    configured: true,
    validation_error: null,
    value: {},
  }));
  vi.spyOn(moduleRuntimeEnvironment, "load").mockImplementation(async (manifest) => {
    const components = Object.fromEntries(manifest.frontend.components.map((name) => [
      name,
      name === "InventoryView"
        ? NeutralInventoryView
        : () => <section data-testid={`${manifest.id}:${name}`} />,
    ]));
    const activation = {
      components,
      primaryActions: manifest.id === "inventory"
        ? [{ component: "InventoryView", label: "Create item" }]
        : undefined,
    };
    return {
      manifest,
      entry: {
        pluginApiVersion: "2.0.0",
        moduleId: manifest.id,
        version: manifest.version,
        activate: () => activation,
      },
      activation,
      dispose: vi.fn(),
    };
  });
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    value: vi.fn().mockReturnValue({ matches: false }),
  });
  return { services };
}

function queryClientFromCall(call: readonly unknown[]): QueryClient {
  const context = call[0] as { client?: unknown } | undefined;
  if (!(context?.client instanceof QueryClient)) throw new Error("query call did not receive a QueryClient");
  return context.client;
}

function seedPrivateCache(queryClient: QueryClient) {
  queryClient.setQueryData(["private-query"], { secret: "cached" });
  queryClient.getMutationCache().build(queryClient, {
    mutationKey: ["private-mutation"],
    mutationFn: async () => undefined,
  });
}

async function submitPasswordChange() {
  fireEvent.click(await screen.findByRole("button", { name: "设置" }));
  const form = await screen.findByRole("form", { name: "修改管理员密码" });
  fireEvent.change(screen.getByLabelText("原密码"), { target: { value: "current-password" } });
  fireEvent.change(screen.getByLabelText("新密码"), { target: { value: "replacement-password" } });
  fireEvent.change(screen.getByLabelText("确认新密码"), { target: { value: "replacement-password" } });
  fireEvent.submit(form);
}

describe("session verification", () => {
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    window.history.replaceState(null, "", "/");
  });

  it("shows the login form only for an unauthenticated response", async () => {
    vi.spyOn(api, "authenticate").mockRejectedValue(new ApiError("not signed in", "unauthorized", 401));
    renderApp();

    expect(await screen.findByRole("form", { name: "登录 UnionC 管理中心" })).toBeTruthy();
    expect(screen.queryByText("无法验证会话")).toBeNull();
  });

  it("shows a retryable session error for network and server failures", async () => {
    vi.spyOn(api, "authenticate").mockRejectedValue(new ApiError("服务暂不可用", "unavailable", 503));
    renderApp();

    expect(await screen.findByRole("heading", { name: "无法验证会话" })).toBeTruthy();
    expect(screen.getByText("服务暂不可用")).toBeTruthy();
    expect(screen.queryByRole("form", { name: "登录 UnionC 管理中心" })).toBeNull();
  });

  it("clears every query and mutation on auth expiry, then remounts observers on a new session client", async () => {
    const { services } = mockAuthenticatedApp();
    vi.spyOn(api, "login").mockResolvedValue({ username: "admin" });
    const { queryClient: parentQueryClient } = renderApp();

    expect(await screen.findByRole("button", { name: "退出登录" })).toBeTruthy();
    const oldQueryClient = queryClientFromCall(services.mock.calls[0] as unknown[]);
    expect(oldQueryClient).not.toBe(parentQueryClient);
    seedPrivateCache(oldQueryClient);

    act(() => window.dispatchEvent(new Event("unionc:auth-expired")));

    const loginForm = await screen.findByRole("form", { name: "登录 UnionC 管理中心" });
    expect(oldQueryClient.getQueryCache().getAll()).toHaveLength(0);
    expect(oldQueryClient.getMutationCache().getAll()).toHaveLength(0);

    fireEvent.change(screen.getByLabelText("密码"), { target: { value: "new-session" } });
    fireEvent.submit(loginForm);

    expect(await screen.findByRole("button", { name: "退出登录" })).toBeTruthy();
    await waitFor(() => expect(services).toHaveBeenCalledTimes(2));
    const newQueryClient = queryClientFromCall(services.mock.calls[1] as unknown[]);
    expect(newQueryClient).not.toBe(oldQueryClient);
    expect(newQueryClient.getQueryData(authQueryKeys.me)).toEqual({ username: "admin", permissions: ["*"] });
  });

  it("enters the signed-out state, clears private caches, and blocks login until logout completes", async () => {
    const { services } = mockAuthenticatedApp();
    let finishLogout!: () => void;
    vi.spyOn(api, "logout").mockReturnValue(new Promise<void>((resolve) => { finishLogout = resolve; }));
    const login = vi.spyOn(api, "login").mockResolvedValue({ username: "admin" });
    renderApp();

    const logout = await screen.findByRole("button", { name: "退出登录" });
    const oldQueryClient = queryClientFromCall(services.mock.calls[0] as unknown[]);
    seedPrivateCache(oldQueryClient);
    fireEvent.click(logout);

    const loginForm = await screen.findByRole("form", { name: "登录 UnionC 管理中心" });
    expect(oldQueryClient.getQueryCache().getAll()).toHaveLength(0);
    expect(oldQueryClient.getMutationCache().getAll()).toHaveLength(0);
    fireEvent.change(screen.getByLabelText("密码"), { target: { value: "new-session" } });
    fireEvent.submit(loginForm);
    expect(login).not.toHaveBeenCalled();
    expect((screen.getByRole("button", { name: "正在退出…" }) as HTMLButtonElement).disabled).toBe(true);

    act(() => finishLogout());
    await waitFor(() => {
      expect((screen.getByRole("button", { name: "登录" }) as HTMLButtonElement).disabled).toBe(false);
    });
    fireEvent.submit(loginForm);

    expect(await screen.findByRole("button", { name: "退出登录" })).toBeTruthy();
    expect(login).toHaveBeenCalledTimes(1);
  });

  it("keeps a late mutation callback on the discarded session client", async () => {
    const { services } = mockAuthenticatedApp();
    let finishLogout!: () => void;
    let finishMutation!: () => void;
    vi.spyOn(api, "logout").mockReturnValue(new Promise<void>((resolve) => { finishLogout = resolve; }));
    vi.spyOn(api, "login").mockResolvedValue({ username: "admin" });
    renderApp();

    const logout = await screen.findByRole("button", { name: "退出登录" });
    const oldQueryClient = queryClientFromCall(services.mock.calls[0] as unknown[]);
    const staleService: ServiceStatus = {
      name: "stale-private-service",
      kind: "test",
      runtime_state: "running",
      healthy: true,
      address: null,
      pid: null,
      message: "old session only",
      updated_at: "2026-08-21T00:00:00Z",
    };
    const staleMutation = oldQueryClient.getMutationCache().build(oldQueryClient, {
      mutationKey: ["late-private-mutation"],
      mutationFn: () => new Promise<void>((resolve) => { finishMutation = resolve; }),
      onSuccess: () => {
        oldQueryClient.setQueryData(overviewQueryKeys.services, [staleService]);
      },
    });
    const mutationResult = staleMutation.execute(undefined);

    fireEvent.click(logout);
    const loginForm = await screen.findByRole("form", { name: "登录 UnionC 管理中心" });
    fireEvent.change(screen.getByLabelText("密码"), { target: { value: "new-session" } });
    act(() => finishLogout());
    await waitFor(() => {
      expect((screen.getByRole("button", { name: "登录" }) as HTMLButtonElement).disabled).toBe(false);
    });
    fireEvent.submit(loginForm);
    expect(await screen.findByRole("button", { name: "退出登录" })).toBeTruthy();
    await waitFor(() => expect(services).toHaveBeenCalledTimes(2));
    const newQueryClient = queryClientFromCall(services.mock.calls[1] as unknown[]);

    await act(async () => {
      finishMutation();
      await mutationResult;
    });

    expect(oldQueryClient.getQueryData(overviewQueryKeys.services)).toEqual([staleService]);
    expect(newQueryClient.getQueryData(overviewQueryKeys.services)).toBeUndefined();
    expect(screen.queryByText("stale-private-service")).toBeNull();
  });

  it("uses the server-side password revocation without a second logout request", async () => {
    mockAuthenticatedApp();
    const logout = vi.spyOn(api, "logout").mockResolvedValue();
    const changePassword = vi.spyOn(api, "changePassword").mockResolvedValue();
    renderApp();

    expect(await screen.findByRole("button", { name: "退出登录" })).toBeTruthy();
    await submitPasswordChange();

    expect(await screen.findByRole("form", { name: "登录 UnionC 管理中心" })).toBeTruthy();
    expect(changePassword).toHaveBeenCalledWith("current-password", "replacement-password");
    expect(logout).not.toHaveBeenCalled();
  });

  it("isolates an invalid dynamic module manifest without losing core navigation", async () => {
    mockAuthenticatedApp();
    vi.mocked(platformApi.modules).mockResolvedValue([...platformModules, invalidRemoteModule]);
    renderApp();

    expect(await screen.findByText(/模块 unsafe-entry Manifest 无效/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "总览" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Unsafe" })).toBeNull();
  });

  it("shows only runtime catalog contributions", async () => {
    mockAuthenticatedApp();
    vi.mocked(platformApi.modules).mockResolvedValue([platformModules[1]]);
    renderApp();

    expect(await screen.findByRole("button", { name: "Reports" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Inventory" })).toBeNull();
  });

  it("filters protected module menus with the authenticated permission grants", async () => {
    mockAuthenticatedApp();
    vi.mocked(api.authenticate).mockResolvedValue({ username: "admin", permissions: [] });
    const protectedInventory: PlatformModule = {
      ...platformModules[0],
      permissions: [{ id: "inventory.private.read", description: "Read private inventory" }],
      frontend: {
        ...platformModules[0].frontend!,
        routes: [{
          ...platformModules[0].frontend!.routes[0],
          permission: "inventory.private.read",
        }],
        menu: [{
          ...platformModules[0].frontend!.menu[0],
          permission: "inventory.private.read",
        }],
      },
    };
    vi.mocked(platformApi.modules).mockResolvedValue([protectedInventory]);
    renderApp();

    expect(await screen.findByRole("button", { name: "总览" })).toBeTruthy();
    await waitFor(() => expect(platformApi.modules).toHaveBeenCalled());
    expect(screen.queryByRole("button", { name: "Inventory" })).toBeNull();
  });

  it("does not replay a consumed module action after returning to the module page", async () => {
    mockAuthenticatedApp();
    renderApp();

    fireEvent.click(await screen.findByRole("button", { name: "Inventory" }));
    expect(await screen.findByRole("heading", { name: "Inventory fixture" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Create item" }));
    expect(await screen.findByText("Handled action 1")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "总览" }));
    fireEvent.click(screen.getByRole("button", { name: "Inventory" }));
    expect(await screen.findByText("Handled action 0")).toBeTruthy();
  });

  it("ignores a password-change callback from a replaced browser session", async () => {
    mockAuthenticatedApp();
    let finishChange!: () => void;
    const changePassword = vi.spyOn(api, "changePassword").mockReturnValue(
      new Promise<void>((resolve) => { finishChange = resolve; }),
    );
    let finishLogout!: () => void;
    const logout = vi.spyOn(api, "logout").mockReturnValue(
      new Promise<void>((resolve) => { finishLogout = resolve; }),
    );
    vi.spyOn(api, "login").mockResolvedValue({ username: "admin" });
    renderApp();

    expect(await screen.findByRole("button", { name: "退出登录" })).toBeTruthy();
    await submitPasswordChange();
    await waitFor(() => expect(changePassword).toHaveBeenCalledTimes(1));

    fireEvent.click(screen.getByRole("button", { name: "退出登录" }));
    const loginForm = await screen.findByRole("form", { name: "登录 UnionC 管理中心" });
    act(() => finishLogout());
    fireEvent.change(screen.getByLabelText("密码"), { target: { value: "new-session-password" } });
    await waitFor(() => {
      expect((screen.getByRole("button", { name: "登录" }) as HTMLButtonElement).disabled).toBe(false);
    });
    fireEvent.submit(loginForm);
    expect(await screen.findByRole("button", { name: "退出登录" })).toBeTruthy();

    await act(async () => {
      finishChange();
      await Promise.resolve();
    });

    expect(screen.getByRole("button", { name: "退出登录" })).toBeTruthy();
    expect(screen.queryByRole("form", { name: "登录 UnionC 管理中心" })).toBeNull();
    expect(logout).toHaveBeenCalledTimes(1);
  });

  it("ignores a late 401 response from a replaced browser session", async () => {
    mockAuthenticatedApp();
    let finishOldRequest!: (response: Response) => void;
    vi.stubGlobal("fetch", vi.fn(() => new Promise<Response>((resolve) => {
      finishOldRequest = resolve;
    })));
    let finishLogout!: () => void;
    vi.spyOn(api, "logout").mockReturnValue(
      new Promise<void>((resolve) => { finishLogout = resolve; }),
    );
    vi.spyOn(api, "login").mockResolvedValue({ username: "admin" });
    renderApp();

    expect(await screen.findByRole("button", { name: "退出登录" })).toBeTruthy();
    const oldRequestOutcome = request("/api/old-session").catch((error: unknown) => error);
    fireEvent.click(screen.getByRole("button", { name: "退出登录" }));
    const loginForm = await screen.findByRole("form", { name: "登录 UnionC 管理中心" });
    act(() => finishLogout());
    fireEvent.change(screen.getByLabelText("密码"), { target: { value: "new-session-password" } });
    await waitFor(() => {
      expect((screen.getByRole("button", { name: "登录" }) as HTMLButtonElement).disabled).toBe(false);
    });
    fireEvent.submit(loginForm);
    expect(await screen.findByRole("button", { name: "退出登录" })).toBeTruthy();

    await act(async () => {
      finishOldRequest(new Response(
        JSON.stringify({ code: "unauthorized", message: "old session expired" }),
        { status: 401, headers: { "Content-Type": "application/json" } },
      ));
      expect(await oldRequestOutcome).toMatchObject({ code: "stale_session", status: 401 });
    });

    expect(screen.getByRole("button", { name: "退出登录" })).toBeTruthy();
    expect(screen.queryByRole("form", { name: "登录 UnionC 管理中心" })).toBeNull();
  });
});
