import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const source = await readFile(new URL("./entry.js", import.meta.url), "utf8");
const remote = await import(
  `data:text/javascript;base64,${Buffer.from(source).toString("base64")}`
);

test("Sunshine entry exposes only its manifest-declared components", () => {
  const activation = remote.default.activate({
    react: {},
    api: { basePath: "/api/modules/sunshine" },
  });

  assert.equal(remote.default.moduleId, "sunshine");
  assert.equal(remote.default.version, "0.5.0");
  assert.equal(remote.default.pluginApiVersion, "1.0.0");
  assert.deepEqual(Object.keys(activation.components).sort(), [
    "SunshineLogsView",
    "SunshineView",
  ]);
});

test("Sunshine loader uses the module-scoped API and accepts a host list", async () => {
  const calls = [];
  const hosts = [{
    id: "living-room",
    name: "Living room",
    host: "127.0.0.1",
    web_port: 47990,
    username: "admin",
    password_set: true,
    verify_tls: true,
    web_url: "https://127.0.0.1:47990",
    probe_status: "complete",
    reachable: true,
    connected: true,
    connection_error: null,
  }];
  const api = {
    request: async (path) => {
      calls.push(path);
      return hosts;
    },
  };

  assert.deepEqual(await remote.loadSunshineHosts(api), hosts);
  assert.deepEqual(calls, ["/hosts"]);
});

test("Sunshine loader rejects malformed responses before rendering", async () => {
  await assert.rejects(
    remote.loadSunshineHosts({ request: async () => [{ id: "missing-fields" }] }),
    /Sunshine 主机列表响应格式无效/,
  );
});
