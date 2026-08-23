// @vitest-environment jsdom

import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AgentActivationPage } from "./AgentActivationPage";
import { agentActivationApi as api } from "./api";

const clients: QueryClient[] = [];

afterEach(() => {
  cleanup();
  for (const client of clients.splice(0)) client.clear();
  vi.restoreAllMocks();
});

function renderPage() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  clients.push(queryClient);
  return {
    queryClient,
    ...render(
      <QueryClientProvider client={queryClient}>
        <AgentActivationPage requestId="pairing-request" />
      </QueryClientProvider>,
    ),
  };
}

function mockWaitingPairing() {
  vi.spyOn(api, "agentPairingRequest").mockResolvedValue({
    request_id: "pairing-request",
    os: "linux",
    arch: "x86_64",
    agent_version: "0.3.3",
    status: "waiting",
    expires_at: "2026-08-22T12:15:00Z",
  });
}

describe("public Agent activation secret lifetime", () => {
  it("removes a successful activation code from the mutation cache", async () => {
    mockWaitingPairing();
    vi.spyOn(api, "activateAgent").mockResolvedValue({ instance_id: "agent-1", status: "active" });
    const { queryClient } = renderPage();
    const user = userEvent.setup();

    const input = await screen.findByLabelText(/一次性激活码/);
    await user.type(input, "activation-secret-123");
    await user.click(screen.getByRole("button", { name: "确认激活" }));

    expect(await screen.findByRole("heading", { name: "Agent 激活成功" })).toBeTruthy();
    expect(api.activateAgent).toHaveBeenCalledWith("pairing-request", "activation-secret-123");
    expect(queryClient.getMutationCache().getAll()).toHaveLength(0);
  });

  it("keeps a retryable input but removes a rejected code from the mutation cache", async () => {
    mockWaitingPairing();
    vi.spyOn(api, "activateAgent").mockRejectedValue(new Error("激活码无效"));
    const { queryClient } = renderPage();
    const user = userEvent.setup();

    const input = await screen.findByLabelText(/一次性激活码/) as HTMLInputElement;
    await user.type(input, "rejected-secret-456");
    await user.click(screen.getByRole("button", { name: "确认激活" }));

    expect((await screen.findByRole("alert")).textContent).toContain("激活码无效");
    expect(input.value).toBe("rejected-secret-456");
    await waitFor(() => expect(queryClient.getMutationCache().getAll()).toHaveLength(0));
  });
});
