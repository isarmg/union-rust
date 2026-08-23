// @vitest-environment jsdom

import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, describe, expect, it, vi } from "vitest";
import { monitoringApi as api } from "../api";
import { monitoringQueryKeys as queryKeys } from "../queryKeys";
import type { CreatedAgentInstance, MonitoringHostSummary } from "../types";
import { AgentInstances, HostRegistration } from "./AgentInstances";

const clients: QueryClient[] = [];

const created: CreatedAgentInstance = {
  request_id: "request-1",
  instance_id: "host-1",
  display_name: "测试主机",
  status: "pending",
  activation_code: "one-time-secret",
  expires_at: "2026-08-21T12:15:00Z",
  created_at: "2026-08-21T12:00:00Z",
};

const host: MonitoringHostSummary = {
  id: "host-1",
  name: "测试主机",
  os: "windows",
  os_version: "11",
  kernel_version: null,
  arch: "x86_64",
  agent_version: "0.3.4",
  registered_at: "2026-08-21T12:00:00Z",
  last_seen_at: "2026-08-21T12:00:00Z",
  latest_collected_at: "2026-08-21T12:00:00Z",
  status: "online",
  capabilities: [],
  cpu_usage_percent: null,
  memory_usage_percent: null,
  network_received_bytes_per_second: null,
  network_transmitted_bytes_per_second: null,
  disk_read_bytes_per_second: null,
  disk_written_bytes_per_second: null,
  max_temperature_celsius: null,
  gpu_utilization_percent: null,
  gpu_memory_usage_percent: null,
};

afterEach(() => {
  cleanup();
  for (const client of clients.splice(0)) client.clear();
  vi.restoreAllMocks();
});

function renderWithClient(node: React.ReactNode) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  clients.push(queryClient);
  const wrap = (next: React.ReactNode) => (
    <QueryClientProvider client={queryClient}>{next}</QueryClientProvider>
  );
  const rendered = render(wrap(node));
  return {
    ...rendered,
    queryClient,
    rerenderWithClient: (next: React.ReactNode) => rendered.rerender(wrap(next)),
  };
}

function activationCodes(queryClient: QueryClient): string[] {
  return queryClient.getMutationCache().getAll().flatMap((mutation) => {
    const data = mutation.state.data;
    if (!data || typeof data !== "object" || !("activation_code" in data)) return [];
    const code = (data as { activation_code?: unknown }).activation_code;
    return typeof code === "string" ? [code] : [];
  });
}

function mockPairingApis() {
  vi.spyOn(api, "monitoringAgentInstances").mockResolvedValue([]);
  vi.spyOn(api, "monitoringCreateAgentInstance").mockResolvedValue(created);
  vi.spyOn(api, "monitoringCancelAgentInstance").mockResolvedValue();
}

function triggerAgentCreation(rerenderWithClient: (node: React.ReactNode) => void) {
  rerenderWithClient(<AgentInstances activeHostIds={new Set()} addTrigger={1} />);
}

async function expectSecretVisible(queryClient: QueryClient) {
  expect(await screen.findByText(created.activation_code)).toBeTruthy();
  expect(activationCodes(queryClient)).toContain(created.activation_code);
  await waitFor(() => expect(queryClient.isMutating()).toBe(0));
}

async function expectSecretCleared(queryClient: QueryClient) {
  await waitFor(() => {
    expect(screen.queryByText(created.activation_code)).toBeNull();
    expect(activationCodes(queryClient)).not.toContain(created.activation_code);
  });
}

describe("Agent activation-code lifetime", () => {
  it("honors an add trigger already present on first mount", async () => {
    mockPairingApis();
    const { queryClient } = renderWithClient(
      <AgentInstances activeHostIds={new Set()} addTrigger={1} />,
    );

    await expectSecretVisible(queryClient);
    expect(api.monitoringCreateAgentInstance).toHaveBeenCalledWith("概览", 15);
  });

  it("clears first-pairing mutation data when the operator closes the panel", async () => {
    mockPairingApis();
    const { queryClient, rerenderWithClient } = renderWithClient(
      <AgentInstances activeHostIds={new Set()} addTrigger={0} />,
    );

    triggerAgentCreation(rerenderWithClient);
    await expectSecretVisible(queryClient);
    fireEvent.click(screen.getByRole("button", { name: "取消邀请并清除授权密钥" }));

    await expectSecretCleared(queryClient);
    expect(api.monitoringCancelAgentInstance).toHaveBeenCalledWith(created.request_id);
  }, 15_000);

  it("clears first-pairing mutation data when the invitation reaches a terminal state", async () => {
    mockPairingApis();
    const { queryClient, rerenderWithClient } = renderWithClient(
      <AgentInstances activeHostIds={new Set()} addTrigger={0} />,
    );

    triggerAgentCreation(rerenderWithClient);
    await expectSecretVisible(queryClient);
    act(() => {
      queryClient.setQueryData(queryKeys.monitoring.agentInstances, [{ ...created, status: "active" }]);
    });

    await expectSecretCleared(queryClient);
  }, 15_000);

  it("clears first-pairing mutation data when the component unmounts", async () => {
    mockPairingApis();
    const { queryClient, unmount, rerenderWithClient } = renderWithClient(
      <AgentInstances activeHostIds={new Set()} addTrigger={0} />,
    );

    triggerAgentCreation(rerenderWithClient);
    await expectSecretVisible(queryClient);
    unmount();

    await waitFor(() => expect(activationCodes(queryClient)).not.toContain(created.activation_code));
  }, 15_000);

});

describe("Agent invitation request lifetime", () => {
  it("aborts the in-flight list request when the creation result unmounts", async () => {
    let requestSignal: AbortSignal | undefined;
    vi.spyOn(api, "monitoringCreateAgentInstance").mockResolvedValue(created);
    vi.spyOn(globalThis, "fetch").mockImplementation((_input, init) => (
      new Promise<Response>((_resolve, reject) => {
        requestSignal = init?.signal ?? undefined;
        requestSignal?.addEventListener("abort", () => {
          reject(new DOMException("aborted", "AbortError"));
        }, { once: true });
      })
    ));
    const { unmount, rerenderWithClient } = renderWithClient(
      <AgentInstances activeHostIds={new Set()} addTrigger={0} />,
    );
    triggerAgentCreation(rerenderWithClient);
    await waitFor(() => expect(requestSignal).toBeDefined());

    unmount();

    expect(requestSignal?.aborted).toBe(true);
    await act(async () => { await Promise.resolve(); });
  });
});

describe("managed monitoring host card", () => {
  it("keeps only detail and deletion actions and omits metric summary rows", () => {
    const { container } = renderWithClient(<HostRegistration host={host} />);
    const card = screen.getByRole("article", { name: /测试主机/ });

    expect(within(card).getByRole("button", { name: "详情" })).toBeTruthy();
    expect(within(card).getByRole("button", { name: "删除" })).toBeTruthy();
    expect(within(card).queryByRole("button", { name: "重新配对" })).toBeNull();
    expect(within(card).queryByRole("button", { name: "撤销" })).toBeNull();
    expect(container.textContent).not.toContain("CPU");
    expect(container.textContent).not.toContain("GPU");
    expect(container.textContent).not.toContain("网络");
  });

  it("edits the name with the same borderless inline interaction as Sunshine cards", async () => {
    vi.spyOn(api, "monitoringUpdateRemark").mockResolvedValue();
    const user = userEvent.setup();
    renderWithClient(<HostRegistration host={host} />);

    const edit = screen.getByRole("button", { name: /修改名称/ });
    expect(edit.classList.contains("sunshine-inline-editable")).toBe(true);
    await user.click(edit);
    const input = screen.getByLabelText("名称");
    expect(input.classList.contains("sunshine-inline-input")).toBe(true);
    await user.clear(input);
    await user.type(input, "客厅工作站");
    await user.keyboard("{Enter}");

    await waitFor(() => expect(api.monitoringUpdateRemark).toHaveBeenCalledWith(host.id, "客厅工作站"));
  });

  it("opens details only from the card action", () => {
    const onOpenDetails = vi.fn();
    renderWithClient(
      <HostRegistration host={host} onOpenDetails={onOpenDetails} />,
    );
    const card = screen.getByRole("article", { name: /测试主机/ });

    fireEvent.click(card);
    expect(onOpenDetails).not.toHaveBeenCalled();

    fireEvent.click(within(card).getByRole("button", { name: "详情" }));
    expect(onOpenDetails).toHaveBeenCalledOnce();
  });

  it("exposes the open state without adding a selected outline class", () => {
    renderWithClient(<HostRegistration host={host} selected />);
    const card = screen.getByRole("article", { name: /测试主机/ });

    expect(card.dataset.detailOpen).toBe("true");
    expect(card.classList.contains("selected")).toBe(false);
    expect(within(card).getByRole("button", { name: "收起详情" })).toBeTruthy();
  });

  it("permanently deletes the instance without opening details", async () => {
    vi.spyOn(api, "monitoringDeleteHost").mockResolvedValue();
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const onDeleted = vi.fn();
    const onOpenDetails = vi.fn();
    renderWithClient(
      <HostRegistration
        host={host}
        onDeleted={onDeleted}
        onOpenDetails={onOpenDetails}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "删除" }));

    expect(onOpenDetails).not.toHaveBeenCalled();
    await waitFor(() => expect(api.monitoringDeleteHost).toHaveBeenCalledWith(host.id));
    await waitFor(() => expect(onDeleted).toHaveBeenCalledOnce());
  });
});
