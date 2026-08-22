// @vitest-environment jsdom

import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { PropsWithChildren } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "./api";
import { useEventStream } from "./hooks";
import { queryKeys } from "./query-keys";
import type { ServiceStatus } from "./types";

class FakeEventSource {
  static instances: FakeEventSource[] = [];

  readonly listeners = new Map<string, Array<(event: { data: string }) => void>>();
  closed = false;

  constructor(readonly url: string) {
    FakeEventSource.instances.push(this);
  }

  addEventListener(type: string, listener: (event: { data: string }) => void) {
    const listeners = this.listeners.get(type) ?? [];
    listeners.push(listener);
    this.listeners.set(type, listeners);
  }

  emit(type: string, data = "") {
    for (const listener of this.listeners.get(type) ?? []) listener({ data });
  }

  close() {
    this.closed = true;
  }
}

describe("useEventStream", () => {
  beforeEach(() => {
    FakeEventSource.instances = [];
    vi.stubGlobal("EventSource", FakeEventSource);
    vi.spyOn(api, "issueSseTicket").mockResolvedValue({ ticket: "short-lived-ticket" });
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("writes status events into the React Query cache and marks a broken stream disconnected", async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const invalidate = vi.spyOn(queryClient, "invalidateQueries");
    const wrapper = ({ children }: PropsWithChildren) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    );
    const { result, unmount } = renderHook(() => useEventStream(), { wrapper });

    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(1));
    const source = FakeEventSource.instances[0];
    expect(source.url).toBe("/api/events?ticket=short-lived-ticket");

    act(() => source.emit("open"));
    expect(result.current.connected).toBe(true);

    const services: ServiceStatus[] = [{
      name: "Sunshine",
      kind: "sunshine",
      healthy: true,
      runtime_state: "running",
      pid: 42,
      address: "127.0.0.1:47990",
      message: "ready",
      updated_at: "2026-08-19T12:00:00Z",
    }];
    act(() => source.emit("status", JSON.stringify({ services })));
    expect(queryClient.getQueryData(queryKeys.services)).toEqual(services);

    act(() => source.emit("error"));
    await waitFor(() => expect(result.current.connected).toBe(false));
    expect(source.closed).toBe(true);
    expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.services });
    unmount();
  });

  it("ignores late callbacks from a replaced EventSource", async () => {
    vi.useFakeTimers();
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const wrapper = ({ children }: PropsWithChildren) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    );
    const { result, unmount } = renderHook(() => useEventStream(), { wrapper });

    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(FakeEventSource.instances).toHaveLength(1);
    const oldSource = FakeEventSource.instances[0];
    act(() => oldSource.emit("open"));
    act(() => oldSource.emit("error"));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
      await Promise.resolve();
    });
    expect(FakeEventSource.instances).toHaveLength(2);
    const currentSource = FakeEventSource.instances[1];
    act(() => currentSource.emit("open"));
    expect(result.current.connected).toBe(true);

    act(() => oldSource.emit("error"));
    expect(currentSource.closed).toBe(false);
    expect(result.current.connected).toBe(true);
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(FakeEventSource.instances).toHaveLength(2);
    unmount();
  });
});
