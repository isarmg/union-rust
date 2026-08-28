import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { authApi } from "../features/auth/api";
import { platformApi } from "../features/platform/api";
import {
  advanceAuthSessionGeneration,
  currentAuthSessionGeneration,
} from "../shared/api/client";

describe("platform API request contracts", () => {
  const dispatchEvent = vi.fn((event: Event) => Boolean(event.type));
  let fetchMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    fetchMock = vi.fn();
    dispatchEvent.mockClear();
    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal("document", { cookie: "" });
    vi.stubGlobal("window", {
      setTimeout: globalThis.setTimeout.bind(globalThis),
      clearTimeout: globalThis.clearTimeout.bind(globalThis),
      dispatchEvent,
    });
  });

  afterEach(() => vi.unstubAllGlobals());

  function jsonResponse(payload: unknown, status = 200): Response {
    return new Response(JSON.stringify(payload), {
      status,
      headers: { "Content-Type": "application/json" },
    });
  }

  it("broadcasts a 401 only for the browser session that issued the request", async () => {
    let resolveResponse!: (response: Response) => void;
    fetchMock.mockReturnValue(new Promise<Response>((resolve) => { resolveResponse = resolve; }));
    const generation = currentAuthSessionGeneration();
    const stale = authApi.logout().catch((error: unknown) => error);
    advanceAuthSessionGeneration();
    resolveResponse(jsonResponse({ code: "unauthorized", message: "expired" }, 401));

    await expect(stale).resolves.toMatchObject({ code: "stale_session", status: 401 });
    expect(dispatchEvent).not.toHaveBeenCalled();

    fetchMock.mockResolvedValueOnce(jsonResponse({ code: "unauthorized", message: "expired" }, 401));
    await expect(authApi.logout()).rejects.toMatchObject({ code: "unauthorized", status: 401 });
    expect(dispatchEvent).toHaveBeenCalledTimes(1);
    expect(dispatchEvent.mock.calls[0]?.[0]).toMatchObject({
      type: "unionc:auth-expired",
      detail: generation + 1,
    });
  });

  it("rejects a successful status outside the current endpoint contract", async () => {
    fetchMock.mockResolvedValue(jsonResponse({ username: "admin" }, 201));

    await expect(authApi.authenticate()).rejects.toMatchObject({
      code: "unexpected_status",
      status: 201,
    });
  });

  it("requires the current application/json response media type", async () => {
    fetchMock.mockResolvedValue(new Response(JSON.stringify({ username: "admin" }), {
      status: 200,
      headers: { "Content-Type": "text/plain" },
    }));

    await expect(authApi.authenticate()).rejects.toMatchObject({
      code: "unexpected_content_type",
      status: 200,
    });
  });

  it("accepts the exact no-content status without a JSON body", async () => {
    fetchMock.mockResolvedValue(new Response(null, { status: 204 }));

    await expect(authApi.logout()).resolves.toBeUndefined();
  });

  it("encodes module ids and applies CSRF to every runtime-management mutation", async () => {
    document.cookie = "csrf=module-csrf-token";
    const configuration = {
      module: "example/module",
      schema_version: 1,
      schema: { type: "object" },
      configured: true,
      validation_error: null,
      value: { endpoint: "/srv/example" },
    } as const;
    fetchMock
      .mockResolvedValueOnce(jsonResponse(configuration))
      .mockResolvedValueOnce(jsonResponse(configuration))
      .mockResolvedValueOnce(jsonResponse([]))
      .mockResolvedValueOnce(jsonResponse([]))
      .mockResolvedValueOnce(jsonResponse([]));

    await platformApi.moduleConfiguration("example/module");
    await platformApi.saveModuleConfiguration("example/module", { endpoint: "/srv/example" });
    await platformApi.enableModule("example/module");
    await platformApi.disableModule("example/module");
    await platformApi.rescanModules();

    expect(fetchMock.mock.calls.map(([path]) => path)).toEqual([
      "/api/platform/modules/example%2Fmodule/configuration",
      "/api/platform/modules/example%2Fmodule/configuration",
      "/api/platform/modules/example%2Fmodule/enable",
      "/api/platform/modules/example%2Fmodule/disable",
      "/api/platform/modules/rescan",
    ]);
    const methods = fetchMock.mock.calls.map(([, init]) => (init as RequestInit).method ?? "GET");
    expect(methods).toEqual(["GET", "PUT", "POST", "POST", "POST"]);
    for (const [, init] of fetchMock.mock.calls.slice(1)) {
      expect((init as RequestInit).headers).toMatchObject({
        "X-CSRF-Token": "module-csrf-token",
      });
    }
    expect(JSON.parse(String((fetchMock.mock.calls[1]?.[1] as RequestInit).body))).toEqual({
      endpoint: "/srv/example",
    });
  });
});
