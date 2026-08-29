// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { authApi as api } from "../auth/api";
import { platformApi } from "../platform/api";
import type { ModuleConfiguration, PlatformModule } from "../platform/types";
import { SettingsView } from "./SettingsView";

const clients: QueryClient[] = [];

const disabledModule: PlatformModule = {
  manifest_version: 2,
  id: "example-module",
  display_name: "Example Module",
  description: "Neutral module fixture",
  version: "0.1.0",
  compatibility: {
    core: ">=0.6.0, <0.7.0",
    platform_api: "^1.0.0",
    plugin_api: "^2.0.0",
  },
  dependencies: [],
  permissions: [{ id: "example-module.read", description: "Read example data" }],
  frontend: {
    entry: "frontend/entry.js",
    styles: ["frontend/styles.css"],
    components: ["ExampleView"],
    api_base: "/api/modules/example-module",
    routes: [{
      path: "/modules/example-module",
      component: "ExampleView",
      permission: "example-module.read",
    }],
    menu: [{
      id: "overview",
      label: "Example",
      route: "/modules/example-module",
      permission: "example-module.read",
      order: 30,
    }],
  },
  enabled: false,
  lifecycle_state: "stopped",
  health_message: "module is included in this distribution but disabled",
  pid: null,
  restart_count: 2,
  checked_at: "2026-08-27T00:00:00Z",
  resolved_frontend: {
    entry: "/modules/example-module/assets/frontend/entry.js",
    styles: ["/modules/example-module/assets/frontend/styles.css"],
  },
};

const unconfigured: ModuleConfiguration = {
  module: "example-module",
  schema_version: 1,
  schema: {
    type: "object",
    properties: { port: { type: "number" } },
    required: ["port"],
  },
  configured: false,
  validation_error: null,
  value: null,
};

function renderSettings() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  clients.push(queryClient);
  const rendered = render(
    <QueryClientProvider client={queryClient}>
      <SettingsView onPasswordChanged={vi.fn()} />
    </QueryClientProvider>,
  );
  return { ...rendered, queryClient };
}

beforeEach(() => {
  vi.spyOn(platformApi, "modules").mockResolvedValue([]);
});

afterEach(() => {
  cleanup();
  for (const client of clients.splice(0)) client.clear();
  vi.restoreAllMocks();
});

describe("administrator password mutation lifetime", () => {
  it("embeds password inputs in rows two through four of the account card", async () => {
    vi.spyOn(api, "authenticate").mockResolvedValue({ username: "admin" });
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    clients.push(queryClient);
    render(
      <QueryClientProvider client={queryClient}>
        <SettingsView onPasswordChanged={vi.fn()} />
      </QueryClientProvider>,
    );

    const form = await screen.findByRole("form", { name: "修改管理员密码" });
    const rows = form.querySelectorAll(".sarmg-card__row");
    expect(rows).toHaveLength(6);
    expect(rows[0].textContent).toContain("用户");
    expect(within(rows[1] as HTMLElement).getByLabelText("原密码")).toBeTruthy();
    expect(within(rows[2] as HTMLElement).getByLabelText("新密码")).toBeTruthy();
    expect(within(rows[3] as HTMLElement).getByLabelText("确认新密码")).toBeTruthy();
    expect(within(rows[5] as HTMLElement).getByRole("button", { name: "修改密码" })).toBeTruthy();
  });

  it("does not retain rejected passwords in the mutation cache", async () => {
    vi.spyOn(api, "authenticate").mockResolvedValue({ username: "admin" });
    vi.spyOn(api, "changePassword").mockRejectedValue(new Error("当前密码错误"));
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    clients.push(queryClient);
    const onPasswordChanged = vi.fn();
    render(
      <QueryClientProvider client={queryClient}>
        <SettingsView onPasswordChanged={onPasswordChanged} />
      </QueryClientProvider>,
    );

    const currentPassword = await screen.findByLabelText("原密码") as HTMLInputElement;
    const newPassword = screen.getByLabelText("新密码") as HTMLInputElement;
    fireEvent.change(currentPassword, { target: { value: "current-secret" } });
    fireEvent.change(newPassword, { target: { value: "replacement-secret" } });
    fireEvent.change(screen.getByLabelText("确认新密码"), { target: { value: "replacement-secret" } });
    fireEvent.submit(screen.getByRole("form", { name: "修改管理员密码" }));

    expect((await screen.findByRole("alert")).textContent).toContain("当前密码错误");
    expect(currentPassword.value).toBe("current-secret");
    expect(newPassword.value).toBe("replacement-secret");
    await waitFor(() => expect(queryClient.getMutationCache().getAll()).toHaveLength(0));
    expect(onPasswordChanged).not.toHaveBeenCalled();
  });
});

describe("runtime module management", () => {
  it("configures a disabled bundled module before enabling it", async () => {
    vi.spyOn(api, "authenticate").mockResolvedValue({ username: "admin" });
    vi.mocked(platformApi.modules).mockResolvedValue([disabledModule]);
    vi.spyOn(platformApi, "moduleConfiguration").mockResolvedValue(unconfigured);
    const savedConfiguration: ModuleConfiguration = {
      ...unconfigured,
      configured: true,
      value: { port: 7100 },
    };
    const save = vi.spyOn(platformApi, "saveModuleConfiguration").mockResolvedValue(savedConfiguration);
    const enable = vi.spyOn(platformApi, "enableModule").mockResolvedValue([{
      ...disabledModule,
      enabled: true,
      lifecycle_state: "available",
      health_message: "worker ready",
      pid: 4102,
    }]);
    renderSettings();

    const card = await screen.findByRole("article", { name: "模块 Example Module" });
    const enableButton = within(card).getByRole("button", { name: "启用模块" }) as HTMLButtonElement;
    expect(enableButton.disabled).toBe(true);
    expect(within(card).getByText("已包含")).toBeTruthy();
    expect(await within(card).findByText("未配置")).toBeTruthy();
    expect(within(card).getByText("2")).toBeTruthy();

    fireEvent.change(within(card).getByLabelText("Example Module 配置 JSON"), {
      target: { value: "{\n  \"port\": 7100\n}" },
    });
    fireEvent.click(within(card).getByRole("button", { name: "保存配置" }));

    await waitFor(() => expect(save).toHaveBeenCalledWith("example-module", { port: 7100 }));
    await waitFor(() => expect(enableButton.disabled).toBe(false));
    fireEvent.click(enableButton);

    await waitFor(() => expect(enable).toHaveBeenCalledWith("example-module"));
    expect(await within(card).findByRole("button", { name: "停用模块" })).toBeTruthy();
    expect(within(card).getByText("4102")).toBeTruthy();
  });

  it("disables a running module and refreshes the catalog from the mutation response", async () => {
    vi.spyOn(api, "authenticate").mockResolvedValue({ username: "admin" });
    const runningModule: PlatformModule = {
      ...disabledModule,
      enabled: true,
      lifecycle_state: "available",
      health_message: "worker ready",
      pid: 4102,
    };
    vi.mocked(platformApi.modules).mockResolvedValue([runningModule]);
    vi.spyOn(platformApi, "moduleConfiguration").mockResolvedValue({
      ...unconfigured,
      configured: true,
      value: { port: 7100 },
    });
    const disable = vi.spyOn(platformApi, "disableModule").mockResolvedValue([disabledModule]);
    renderSettings();

    const card = await screen.findByRole("article", { name: "模块 Example Module" });
    fireEvent.click(within(card).getByRole("button", { name: "停用模块" }));

    await waitFor(() => expect(disable).toHaveBeenCalledWith("example-module"));
    expect(await within(card).findByRole("button", { name: "启用模块" })).toBeTruthy();
    expect(within(card).getByText("未启用")).toBeTruthy();
  });

  it("rescans only bundled packages and renders the returned disabled module", async () => {
    vi.spyOn(api, "authenticate").mockResolvedValue({ username: "admin" });
    vi.spyOn(platformApi, "moduleConfiguration").mockResolvedValue(unconfigured);
    const rescan = vi.spyOn(platformApi, "rescanModules").mockResolvedValue([disabledModule]);
    renderSettings();

    expect(await screen.findByText("当前发行未包含业务模块。")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "重新扫描" }));

    await waitFor(() => expect(rescan).toHaveBeenCalledTimes(1));
    expect(await screen.findByRole("article", { name: "模块 Example Module" })).toBeTruthy();
    expect(screen.queryByText(/安装|升级|卸载|上传模块包/)).toBeNull();
  });

  it("surfaces catalog and configuration failures without hiding bundled modules", async () => {
    vi.spyOn(api, "authenticate").mockResolvedValue({ username: "admin" });
    vi.mocked(platformApi.modules).mockResolvedValue([disabledModule]);
    vi.spyOn(platformApi, "moduleConfiguration").mockRejectedValue(new Error("configuration unavailable"));
    renderSettings();

    const card = await screen.findByRole("article", { name: "模块 Example Module" });
    expect(await within(card).findByText("配置读取失败：configuration unavailable")).toBeTruthy();
    expect((within(card).getByRole("button", { name: "启用模块" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("blocks redacted secret placeholders until complete replacement is explicit", async () => {
    vi.spyOn(api, "authenticate").mockResolvedValue({ username: "admin" });
    vi.mocked(platformApi.modules).mockResolvedValue([disabledModule]);
    vi.spyOn(platformApi, "moduleConfiguration").mockResolvedValue({
      ...unconfigured,
      configured: true,
      schema: {
        type: "object",
        properties: { token: { type: "string" }, port: { type: "number" } },
        required: ["token", "port"],
      },
      value: { token: "***", port: 7100 },
    });
    const save = vi.spyOn(platformApi, "saveModuleConfiguration").mockResolvedValue({
      ...unconfigured,
      configured: true,
      value: { token: "***", port: 7100 },
    });
    const { queryClient } = renderSettings();

    const card = await screen.findByRole("article", { name: "模块 Example Module" });
    const saveButton = within(card).getByRole("button", { name: "保存配置" }) as HTMLButtonElement;
    expect(await within(card).findByText(/必须替换所有占位符/)).toBeTruthy();
    expect(within(card).getByText(/当前禁止保存/)).toBeTruthy();
    expect(saveButton.disabled).toBe(true);

    fireEvent.change(within(card).getByLabelText("Example Module 配置 JSON"), {
      target: { value: "{\"token\":\"complete-new-secret\",\"port\":7100}" },
    });
    expect(saveButton.disabled).toBe(true);
    fireEvent.click(within(card).getByLabelText("我已为所有隐藏字段填写完整的新值"));
    expect(saveButton.disabled).toBe(false);
    fireEvent.click(saveButton);

    await waitFor(() => expect(save).toHaveBeenCalledWith("example-module", {
      token: "complete-new-secret",
      port: 7100,
    }));
    await waitFor(() => expect(queryClient.getMutationCache().getAll()).toHaveLength(0));
  });
});
