// @vitest-environment jsdom

import { cleanup, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { api, ApiError } from "./api";

function renderApp() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <App />
    </QueryClientProvider>,
  );
}

describe("session verification", () => {
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
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
});
