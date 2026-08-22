import { describe, expect, it } from "vitest";
import {
  isOptimisticSunshineHost,
  mergeSunshineHostSnapshot,
  optimisticSunshineHost,
  parseSunshineConfigDraft,
  persistedSunshineHosts,
  removeSunshineHost,
  replaceSunshineHost,
  restoreSunshineHost,
  sunshineHostsRefetchInterval,
  sunshineLogLines,
} from "./data";
import type { SunshineHostInfo } from "./types";

function host(id: string, name = id): SunshineHostInfo {
  return {
    id,
    name,
    host: `${id}.example.test`,
    web_port: 47990,
    username: "admin",
    password_set: true,
    verify_tls: true,
    web_url: `https://${id}.example.test:47990`,
    probe_status: "complete",
    reachable: true,
    connected: true,
  };
}

describe("Sunshine current response contract", () => {
  it("reads the UnionC content wrapper around Sunshine text logs", () => {
    expect(sunshineLogLines({ content: "first\r\nsecond" })).toEqual(["first", "second"]);
  });

  it("preserves JSON config value types", () => {
    const config = parseSunshineConfigDraft(
      '{"enabled":true,"port":47990,"nested":{"mode":"safe"}}',
    );
    expect(config).toEqual({
      enabled: true,
      port: 47990,
      nested: { mode: "safe" },
    });
  });

  it("rejects non-object config roots", () => {
    expect(() => parseSunshineConfigDraft("[]")).toThrow("JSON 对象");
    expect(() => parseSunshineConfigDraft('"text"')).toThrow("JSON 对象");
  });

  it("creates an immediately visible, explicitly pending host entry", () => {
    const pending = optimisticSunshineHost({
      name: "  Living room  ",
      host: "2001:db8::1",
      web_port: 47990,
      username: " admin ",
      password: "secret",
      verify_tls: true,
    });

    expect(isOptimisticSunshineHost(pending)).toBe(true);
    expect(pending).toMatchObject({
      name: "Living room",
      host: "2001:db8::1",
      username: "admin",
      password_set: true,
      probe_status: "pending",
      connected: null,
      web_url: "https://[2001:db8::1]:47990",
    });
  });

  it("reconciles a create response without duplicating the real host", () => {
    const pending = optimisticSunshineHost({
      name: "Pending",
      host: "pending.example.test",
      web_port: 47990,
      username: "admin",
      verify_tls: true,
    });
    const real = host("real-id", "Saved");

    expect(replaceSunshineHost([host("first"), pending], real, pending.id))
      .toEqual([host("first"), real]);
    expect(replaceSunshineHost([real, pending], real, pending.id))
      .toEqual([real]);
  });

  it("pauses list polling for a local create, then polls a real pending probe", () => {
    const pending = optimisticSunshineHost({
      name: "Pending",
      host: "pending.example.test",
      web_port: 47990,
      username: "admin",
      verify_tls: true,
    });
    const serverPending = {
      ...host("server-pending"),
      probe_status: "pending" as const,
      reachable: null,
      connected: null,
    };

    expect(sunshineHostsRefetchInterval([pending])).toBe(false);
    expect(sunshineHostsRefetchInterval([host("deleting")], true)).toBe(false);
    expect(sunshineHostsRefetchInterval([serverPending])).toBe(1_500);
    expect(sunshineHostsRefetchInterval([host("complete")])).toBe(30_000);
  });

  it("excludes local create placeholders from host-specific API consumers", () => {
    const pending = optimisticSunshineHost({
      name: "Pending",
      host: "pending.example.test",
      web_port: 47990,
      username: "admin",
      verify_tls: true,
    });
    const persisted = host("persisted");

    expect(persistedSunshineHosts([pending, persisted])).toEqual([persisted]);
  });

  it("merges a manual refresh around in-flight creates and deletes", () => {
    const kept = host("kept");
    const deleting = host("deleting");
    const staleLocalOnly = host("already-removed-remotely");
    const pending = optimisticSunshineHost({
      name: "Pending",
      host: "pending.example.test",
      web_port: 47990,
      username: "admin",
      verify_tls: true,
    });

    expect(mergeSunshineHostSnapshot(
      [kept, deleting],
      [kept, deleting, staleLocalOnly, pending],
      new Set([deleting.id]),
    )).toEqual([kept, pending]);
  });

  it("rolls back only a failed deletion and preserves concurrent edits", () => {
    const first = host("first");
    const removed = host("removed");
    const concurrentlyAdded = host("new");
    const afterDelete = removeSunshineHost([first, removed], removed.id);

    expect(restoreSunshineHost([...afterDelete, concurrentlyAdded], removed, 1))
      .toEqual([first, removed, concurrentlyAdded]);
  });
});
