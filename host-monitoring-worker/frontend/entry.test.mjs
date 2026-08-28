import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const source = await readFile(new URL("./entry.js", import.meta.url), "utf8");
const remote = await import(
  `data:text/javascript;base64,${Buffer.from(source).toString("base64")}`
);

test("host-monitoring entry exposes its manifest-declared dynamic components", () => {
  const activation = remote.default.activate({
    react: {},
    api: { basePath: "/api/modules/host-monitoring" },
  });

  assert.equal(remote.default.moduleId, "host-monitoring");
  assert.equal(remote.default.version, "0.5.0");
  assert.equal(remote.default.pluginApiVersion, "1.0.0");
  assert.deepEqual(Object.keys(activation.components), ["HostMonitoringView", "HostActivationView"]);
});

test("activation consumes Shell location and module API contracts", async () => {
  const requestId = "00000000-0000-4000-8000-000000000001";
  assert.equal(remote.activationRequestId({
    pathname: `/modules/host-monitoring/activate/${requestId}`,
    params: { requestId },
  }), requestId);
  assert.equal(remote.activationRequestId({ params: { requestId: "not-a-uuid" } }), null);
  assert.equal(remote.activationCodeForSubmission("  one-time-code\n"), "one-time-code");

  const calls = [];
  const api = {
    request: async (path, init) => {
      calls.push([path, init]);
      if (path.startsWith("/agent/v2/pairing-requests/")) {
        return {
          request_id: requestId,
          os: "linux",
          arch: "x86_64",
          agent_version: "0.5.0",
          status: "waiting",
          expires_at: "2026-08-27T12:15:00Z",
        };
      }
      return { instance_id: "instance-one", status: "active" };
    },
  };
  assert.equal((await remote.loadPairing(api, requestId)).request_id, requestId);
  assert.deepEqual(await remote.activatePairing(api, requestId, "one-time-code"), {
    instance_id: "instance-one",
    status: "active",
  });
  assert.deepEqual(calls, [
    [`/agent/v2/pairing-requests/${requestId}`, undefined],
    ["/agent/v2/activate-admin", {
      method: "POST",
      body: JSON.stringify({ request_id: requestId, activation_code: "one-time-code" }),
      suppressAuthExpired: true,
    }],
  ]);
});

test("activation rejects malformed worker responses", async () => {
  await assert.rejects(
    remote.loadPairing({ request: async () => ({ request_id: "wrong" }) },
      "00000000-0000-4000-8000-000000000001"),
    /配对响应格式无效/,
  );
  await assert.rejects(
    remote.activatePairing({ request: async () => ({ status: "active" }) },
      "00000000-0000-4000-8000-000000000001", "code"),
    /激活响应格式无效/,
  );
});

test("host overview loader requests hosts and instances through the module API", async () => {
  const calls = [];
  const hosts = [{
    id: "host-one",
    name: "Build host",
    os: "linux",
    arch: "x86_64",
    agent_version: "0.5.0",
    status: "active",
    last_seen_at: "2026-08-27T12:00:00Z",
  }];
  const instances = [{
    request_id: "request-one",
    instance_id: "instance-one",
    display_name: "Build host",
    status: "pending",
    expires_at: "2026-08-27T12:15:00Z",
    created_at: "2026-08-27T12:00:00Z",
  }];
  const api = {
    request: async (path) => {
      calls.push(path);
      if (path === "/hosts") return { hosts, total: 1, limit: 100, offset: 0 };
      if (path === "/agent-instances") return instances;
      throw new Error("unexpected path");
    },
  };

  assert.deepEqual(await remote.loadHostOverview(api), {
    hosts,
    instances,
    total: 1,
  });
  assert.deepEqual(calls, ["/hosts", "/agent-instances"]);
});

test("host overview loader rejects malformed responses before rendering", async () => {
  const api = {
    request: async (path) => path === "/hosts"
      ? { hosts: [], total: "one" }
      : [],
  };
  await assert.rejects(
    remote.loadHostOverview(api),
    /主机模块概览响应格式无效/,
  );
});
