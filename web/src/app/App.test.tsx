// @vitest-environment jsdom

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { authApi as api } from "../features/auth/api";
import { authQueryKeys } from "../features/auth/queryKeys";
import { overviewApi } from "../features/overview/api";
import { overviewQueryKeys } from "../features/overview/queryKeys";
import type { ServiceStatus } from "../features/overview/types";
import { monitoringApi } from "../features/monitoring/api";
import type { CreatedAgentInstance } from "../features/monitoring/types";
import { ApiError, request } from "../shared/api/client";
import { realtimeApi } from "./realtimeApi";
import { platformApi } from "../features/platform/api";
import type { PlatformModule } from "../features/platform/types";

const platformModules: PlatformModule[] = [
  {
    schema_version: 1,
    id: "host-monitoring",
    display_name: "主机",
    description: "主机监控",
    version: "0.1.0",
    execution: "in_process",
    ui: { kind: "embedded", route: "/modules/host-monitoring" },
    capabilities: [],
    service: null,
    database: {},
    configured: true,
    health: "available",
    health_message: "已编译到当前发行版",
    launch_url: null,
    checked_at: null,
  },
  {
    schema_version: 1,
    id: "sunshine",
    display_name: "Sunshine",
    description: "Sunshine 管理",
    version: "0.1.0",
    execution: "in_process",
    ui: { kind: "embedded", route: "/modules/sunshine" },
    capabilities: [],
    service: null,
    database: {},
    configured: true,
    health: "available",
    health_message: "已编译到当前发行版",
    launch_url: null,
    checked_at: null,
  },
];

const sentinelModule: PlatformModule = {
  schema_version: 1,
  id: "sentinel-monitor",
  display_name: "Sentinel",
  description: "摄像头监控",
  version: "0.1.0",
  execution: "service",
  ui: { kind: "external", public_url_env: "SARMG_SENTINEL_URL" },
  capabilities: [],
  service: {
    base_url_env: "SARMG_SENTINEL_URL",
    liveness_path: "/health/live",
    readiness_path: "/health/ready",
  },
  database: {},
  configured: true,
  health: "available",
  health_message: "公开存活探测通过",
  launch_url: "https://sentinel.example.test/",
  checked_at: "2026-08-27T00:00:00Z",
};

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
  vi.spyOn(api, "authenticate").mockResolvedValue({ username: "admin" });
  const services = vi.spyOn(overviewApi, "services").mockReturnValue(new Promise(() => {}));
  vi.spyOn(overviewApi, "systemResources").mockReturnValue(new Promise(() => {}));
  vi.spyOn(realtimeApi, "issueSseTicket").mockReturnValue(new Promise(() => {}));
  vi.spyOn(platformApi, "modules").mockResolvedValue(platformModules);
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
    expect(newQueryClient.getQueryData(authQueryKeys.me)).toEqual({ username: "admin" });
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

  it("opens configured service modules without forwarding the platform session", async () => {
    mockAuthenticatedApp();
    vi.mocked(platformApi.modules).mockResolvedValue([...platformModules, sentinelModule]);
    renderApp();

    const link = await screen.findByRole("link", { name: "Sentinel" });
    expect(link.getAttribute("href")).toBe("https://sentinel.example.test/");
    expect(link.getAttribute("target")).toBe("_blank");
    expect(link.getAttribute("rel")).toContain("noreferrer");
  });

  it("does not replay a consumed Agent creation request after returning to the host page", async () => {
    mockAuthenticatedApp();
    vi.spyOn(monitoringApi, "monitoringHosts").mockResolvedValue({
      hosts: [],
      total: 0,
      limit: 20,
      offset: 0,
    });
    vi.spyOn(monitoringApi, "monitoringAgentInstances").mockResolvedValue([]);
    const invitationCreatedAt = Date.now();
    const invitation: CreatedAgentInstance = {
      request_id: "request-1",
      instance_id: "host-1",
      display_name: "概览",
      status: "pending",
      activation_code: "one-time-secret",
      expires_at: new Date(invitationCreatedAt + 15 * 60 * 1_000).toISOString(),
      created_at: new Date(invitationCreatedAt).toISOString(),
    };
    const create = vi.spyOn(monitoringApi, "monitoringCreateAgentInstance")
      .mockResolvedValue(invitation);
    const cancel = vi.spyOn(monitoringApi, "monitoringCancelAgentInstance").mockResolvedValue();
    renderApp();

    fireEvent.click(await screen.findByRole("button", { name: "主机" }));
    await screen.findByRole("heading", { name: "主机监控" }, { timeout: 5_000 });
    fireEvent.click(screen.getByRole("button", { name: "创建 Agent" }));
    expect(await screen.findByText(invitation.activation_code, {}, { timeout: 5_000 })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "取消邀请并清除授权密钥" }));
    await waitFor(() => expect(cancel).toHaveBeenCalledWith(invitation.request_id));
    await waitFor(() => expect(screen.queryByText(invitation.activation_code)).toBeNull());

    fireEvent.click(screen.getByRole("button", { name: "总览" }));
    fireEvent.click(screen.getByRole("button", { name: "主机" }));
    await screen.findByRole("heading", { name: "主机监控" });

    expect(create).toHaveBeenCalledTimes(1);
    expect(screen.queryByText(invitation.activation_code)).toBeNull();
    expect(screen.queryByText(/只读采集/)).toBeNull();
    expect(screen.queryByText("暂无 Agent 上报数据")).toBeNull();
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
