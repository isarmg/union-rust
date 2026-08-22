// @vitest-environment jsdom

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
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
  agent_version: "0.3.2",
  lifecycle_status: "active",
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
  const rendered = render(<QueryClientProvider client={queryClient}>{node}</QueryClientProvider>);
  return { ...rendered, queryClient };
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
  it("clears first-pairing mutation data when the operator closes the panel", async () => {
    mockPairingApis();
    const { queryClient } = renderWithClient(<AgentInstances activeHostIds={new Set()} />);

    fireEvent.click(screen.getByRole("button", { name: "创建 Agent" }));
    await expectSecretVisible(queryClient);
    fireEvent.click(screen.getByRole("button", { name: "关闭并清除授权密钥" }));

    await expectSecretCleared(queryClient);
  }, 15_000);

  it("clears first-pairing mutation data when the invitation reaches a terminal state", async () => {
    mockPairingApis();
    const { queryClient } = renderWithClient(<AgentInstances activeHostIds={new Set()} />);

    fireEvent.click(screen.getByRole("button", { name: "创建 Agent" }));
    await expectSecretVisible(queryClient);
    act(() => {
      queryClient.setQueryData(queryKeys.monitoring.agentInstances, [{ ...created, status: "active" }]);
    });

    await expectSecretCleared(queryClient);
  }, 15_000);

  it("clears first-pairing mutation data when the component unmounts", async () => {
    mockPairingApis();
    const { queryClient, unmount } = renderWithClient(
      <AgentInstances activeHostIds={new Set()} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "创建 Agent" }));
    await expectSecretVisible(queryClient);
    unmount();

    await waitFor(() => expect(activationCodes(queryClient)).not.toContain(created.activation_code));
  }, 15_000);

  it("clears re-pairing mutation data when the operator closes the panel", async () => {
    mockPairingApis();
    const { queryClient } = renderWithClient(<HostRegistration host={host} />);

    fireEvent.click(screen.getByRole("button", { name: "重新配对" }));
    await expectSecretVisible(queryClient);
    fireEvent.click(screen.getByRole("button", { name: "关闭并清除授权密钥" }));

    await expectSecretCleared(queryClient);
  }, 15_000);

  it("subscribes re-pairing to invitation status and clears terminal mutation data", async () => {
    mockPairingApis();
    const { queryClient } = renderWithClient(<HostRegistration host={host} />);

    fireEvent.click(screen.getByRole("button", { name: "重新配对" }));
    await expectSecretVisible(queryClient);
    act(() => {
      queryClient.setQueryData(queryKeys.monitoring.agentInstances, [{ ...created, status: "expired" }]);
    });

    await expectSecretCleared(queryClient);
  }, 15_000);

  it("clears re-pairing mutation data when the component unmounts", async () => {
    mockPairingApis();
    const { queryClient, unmount } = renderWithClient(<HostRegistration host={host} />);

    fireEvent.click(screen.getByRole("button", { name: "重新配对" }));
    await expectSecretVisible(queryClient);
    unmount();

    await waitFor(() => expect(activationCodes(queryClient)).not.toContain(created.activation_code));
  }, 15_000);
});
