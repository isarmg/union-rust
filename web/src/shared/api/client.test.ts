import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { request } from "./client";

describe("API response body lifetime", () => {
  let fetchMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    vi.useFakeTimers();
    fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal("document", { cookie: "" });
    vi.stubGlobal("window", {
      setTimeout: globalThis.setTimeout.bind(globalThis),
      clearTimeout: globalThis.clearTimeout.bind(globalThis),
      dispatchEvent: vi.fn(),
    });
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  function responseWhoseBodyEndsOnlyOnAbort(status = 200): Response {
    const [, init] = fetchMock.mock.calls.at(-1) as [string, RequestInit];
    const signal = init.signal;
    const body = new ReadableStream<Uint8Array>({
      start(streamController) {
        const abortBody = () => streamController.error(new DOMException("aborted", "AbortError"));
        if (signal?.aborted) abortBody();
        else signal?.addEventListener("abort", abortBody, { once: true });
      },
    });
    return new Response(body, {
      status,
      headers: { "Content-Type": "application/json" },
    });
  }

  it("keeps the timeout active until a successful JSON body is fully consumed", async () => {
    fetchMock.mockImplementation(async () => responseWhoseBodyEndsOnlyOnAbort());

    const pending = request<{ ok: boolean }>("/api/test", { timeoutMs: 25 });
    const rejected = expect(pending).rejects.toThrow("请求超时，请检查 UnionC 是否可用");
    await vi.advanceTimersByTimeAsync(25);

    await rejected;
  });

  it("forwards caller cancellation after headers while reading an error body", async () => {
    fetchMock.mockImplementation(async () => responseWhoseBodyEndsOnlyOnAbort(502));
    const caller = new AbortController();
    const pending = request("/api/test", { signal: caller.signal, timeoutMs: 0 });
    const rejected = expect(pending).rejects.toThrow("请求已取消");
    await Promise.resolve();
    await Promise.resolve();

    caller.abort();

    await rejected;
  });
});
