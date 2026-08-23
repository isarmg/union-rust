import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { authApi } from "../features/auth/api";
import { agentActivationApi } from "../features/agent-activation/api";
import { sunshineApi } from "../features/sunshine/api";
import { monitoringApi } from "../features/monitoring/api";
import {
  advanceAuthSessionGeneration,
  currentAuthSessionGeneration,
} from "../shared/api/client";

const api = { ...authApi, ...agentActivationApi, ...monitoringApi, ...sunshineApi };

describe("API request contracts", () => {
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

  it("posts only the pairing request id and one-time activation code", async () => {
    fetchMock.mockResolvedValue(jsonResponse({
      instance_id: "f84a2e90-a0d2-445b-907d-019a028bcabc",
      status: "active",
    }));

    await expect(api.activateAgent("pairing-request", "one-time-code")).resolves.toEqual({
      instance_id: "f84a2e90-a0d2-445b-907d-019a028bcabc",
      status: "active",
    });
    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(String(init.body))).toEqual({
      request_id: "pairing-request",
      activation_code: "one-time-code",
    });
  });

  it("does not turn an invalid public activation code into a console logout", async () => {
    fetchMock.mockResolvedValue(jsonResponse({
      code: "unauthorized",
      message: "unauthorized",
    }, 401));

    await expect(api.activateAgent("pairing-request", "bad-code"))
      .rejects.toThrow("激活码无效或已过期");
    expect(dispatchEvent).not.toHaveBeenCalled();
  });

  it("broadcasts a 401 only for the browser session that issued the request", async () => {
    let resolveResponse!: (response: Response) => void;
    fetchMock.mockReturnValue(new Promise<Response>((resolve) => { resolveResponse = resolve; }));
    const generation = currentAuthSessionGeneration();
    const stale = api.logout().catch((error: unknown) => error);
    advanceAuthSessionGeneration();
    resolveResponse(jsonResponse({ code: "unauthorized", message: "expired" }, 401));

    await expect(stale).resolves.toMatchObject({ code: "stale_session", status: 401 });
    expect(dispatchEvent).not.toHaveBeenCalled();

    fetchMock.mockResolvedValueOnce(jsonResponse({ code: "unauthorized", message: "expired" }, 401));
    await expect(api.logout()).rejects.toMatchObject({ code: "unauthorized", status: 401 });
    expect(dispatchEvent).toHaveBeenCalledTimes(1);
    expect(dispatchEvent.mock.calls[0]?.[0]).toMatchObject({
      type: "unionc:auth-expired",
      detail: generation + 1,
    });
  });

  it("reads the limited public pairing summary without requiring a console session", async () => {
    fetchMock.mockResolvedValue(jsonResponse({
      request_id: "pairing-request",
      os: "linux",
      arch: "x86_64",
      agent_version: "0.3.4",
      status: "waiting",
      expires_at: "2026-08-15T12:00:00Z",
    }));

    const summary = await api.agentPairingRequest("pairing/request");
    expect(summary).toMatchObject({ status: "waiting" });
    expect(summary).not.toHaveProperty("name");
    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      "/api/agent/v2/pairing-requests/pairing%2Frequest",
    );
  });

  it("patches only the changed Sunshine host fields", async () => {
    fetchMock.mockResolvedValue(jsonResponse({
      id: "sunshine-one",
      name: "Living room",
      host: "sunshine.example.test",
      web_port: 47990,
      username: "admin",
      password_set: true,
      verify_tls: true,
      web_url: "https://sunshine.example.test:47990",
      probe_status: "complete",
      reachable: true,
      connected: true,
    }));

    await api.sunshineUpdateHost("sunshine/one", { name: "Living room" });

    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe("/api/services/sunshine/hosts/sunshine%2Fone");
    expect(init.method).toBe("PATCH");
    expect(JSON.parse(String(init.body))).toEqual({ name: "Living room" });
  });

  it("normalizes current Sunshine collection responses at the API boundary", async () => {
    fetchMock
      .mockResolvedValueOnce(jsonResponse({
        apps: [
          { name: "Desktop", cmd: null, vendor_extension: { enabled: true } },
          { name: "Game", cmd: "game.exe" },
        ],
        ignored: true,
      }))
      .mockResolvedValueOnce(jsonResponse({
        status: false,
        named_certs: [
          { name: null, uuid: "client-one", enabled: true, vendor_extension: 7 },
        ],
        ignored: true,
      }));

    await expect(api.sunshineApps("sunshine/one")).resolves.toEqual({
      apps: [
        {
          name: "Desktop",
          cmd: null,
          vendor_extension: { enabled: true },
          index: 0,
        },
        { name: "Game", cmd: "game.exe", index: 1 },
      ],
    });
    await expect(api.sunshineClients("sunshine/one")).resolves.toEqual({
      status: false,
      named_certs: [
        { name: null, uuid: "client-one", enabled: true, vendor_extension: 7 },
      ],
    });
    expect(fetchMock.mock.calls.map(([path]) => path)).toEqual([
      "/api/services/sunshine/hosts/sunshine%2Fone/apps",
      "/api/services/sunshine/hosts/sunshine%2Fone/clients",
    ]);
  });

  it("rejects malformed Sunshine application collections before rendering", async () => {
    const malformed = [
      { apps: null },
      { apps: [null] },
      { apps: [{ cmd: "missing name" }] },
      { apps: [{ name: "Desktop", cmd: {} }] },
      { apps: [{ name: "x".repeat(1_025) }] },
      { apps: Array.from({ length: 513 }, () => ({ name: "Desktop" })) },
    ];

    for (const payload of malformed) {
      fetchMock.mockResolvedValueOnce(jsonResponse(payload));
      await expect(api.sunshineApps("sunshine-one"))
        .rejects.toThrow("Sunshine 应用列表响应格式无效");
    }
  });

  it("rejects malformed Sunshine client collections before rendering", async () => {
    const malformed = [
      { status: true, named_certs: null },
      { status: true, named_certs: [null] },
      { status: true, named_certs: [{ uuid: "missing-enabled" }] },
      { status: true, named_certs: [{ uuid: "client", enabled: "yes" }] },
      { status: true, named_certs: [{ uuid: "x".repeat(129), enabled: true }] },
      {
        status: true,
        named_certs: [
          { uuid: "duplicate", enabled: true },
          { uuid: "duplicate", enabled: false },
        ],
      },
      {
        status: true,
        named_certs: Array.from(
          { length: 513 },
          (_, index) => ({ uuid: `client-${index}`, enabled: true }),
        ),
      },
    ];

    for (const payload of malformed) {
      fetchMock.mockResolvedValueOnce(jsonResponse(payload));
      await expect(api.sunshineClients("sunshine-one"))
        .rejects.toThrow("Sunshine 客户端列表响应格式无效");
    }
  });

  it("renames and permanently deletes an encoded monitoring host", async () => {
    fetchMock
      .mockResolvedValueOnce(new Response(null, { status: 204 }))
      .mockResolvedValueOnce(new Response(null, { status: 204 }));

    await api.monitoringUpdateRemark("host/one", "客厅工作站");
    await api.monitoringDeleteHost("host/one");

    const [renamePath, renameInit] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(renamePath).toBe("/api/monitoring/managed-instances/host%2Fone");
    expect(renameInit.method).toBe("PATCH");
    expect(JSON.parse(String(renameInit.body))).toEqual({ remark: "客厅工作站" });
    const [deletePath, deleteInit] = fetchMock.mock.calls[1] as [string, RequestInit];
    expect(deletePath).toBe("/api/monitoring/managed-instances/host%2Fone");
    expect(deleteInit.method).toBe("DELETE");
  });

  it("rejects a successful status outside the current endpoint contract", async () => {
    fetchMock.mockResolvedValue(jsonResponse({ username: "admin" }, 201));

    await expect(api.authenticate()).rejects.toMatchObject({
      code: "unexpected_status",
      status: 201,
    });
  });

  it("requires the current application/json response media type", async () => {
    fetchMock.mockResolvedValue(new Response(JSON.stringify({ username: "admin" }), {
      status: 200,
      headers: { "Content-Type": "text/plain" },
    }));

    await expect(api.authenticate()).rejects.toMatchObject({
      code: "unexpected_content_type",
      status: 200,
    });
  });

  it("accepts the exact no-content status without a JSON body", async () => {
    fetchMock.mockResolvedValue(new Response(null, { status: 204 }));

    await expect(api.logout()).resolves.toBeUndefined();
  });
});
