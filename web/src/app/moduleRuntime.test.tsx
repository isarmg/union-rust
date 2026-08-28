// @vitest-environment jsdom

import * as React from "react";
import { describe, expect, it, vi } from "vitest";
import type { PlatformModule } from "./moduleCatalog";
import { parseModuleCatalog, resolveModuleAsset } from "./moduleCatalog";
import {
  createModuleApi,
  createPermissionChecker,
  loadWebModule,
  matchModuleRoute,
  satisfiesSemverRange,
} from "./moduleRuntime";
import { canLoadModuleFrontend } from "./useModuleRuntime";

function manifest(overrides: Partial<PlatformModule> = {}): PlatformModule {
  return {
    manifest_version: 1,
    id: "example",
    display_name: "Example",
    description: "Example runtime module",
    version: "1.2.3",
    compatibility: {
      core: ">=0.5.0, <0.6.0",
      platform_api: "^1.0.0",
      plugin_api: "^1.0.0",
    },
    dependencies: [],
    permissions: [{ id: "example.read", description: "Read Example" }],
    frontend: {
      entry: "frontend/remoteEntry.js",
      styles: ["frontend/module.css"],
      components: ["ExampleView"],
      api_base: "/api/modules/example",
      routes: [{ path: "/modules/example/:item", component: "ExampleView", permission: "example.read" }],
      menu: [],
    },
    enabled: true,
    lifecycle_state: "available",
    health_message: "ready",
    pid: 10,
    restart_count: 0,
    checked_at: null,
    resolved_frontend: {
      entry: "/modules/example/assets/frontend/remoteEntry.js",
      styles: ["/modules/example/assets/frontend/module.css"],
    },
    ...overrides,
  };
}

describe("runtime Web module contract", () => {
  it("loads same-origin CSS and ESM, and activates with the Shell's only React instance", async () => {
    const disposeStyle = vi.fn();
    const loadStylesheet = vi.fn().mockResolvedValue(disposeStyle);
    let receivedReact: unknown;
    const ExampleView = () => React.createElement("p", null, "example");
    const importModule = vi.fn().mockResolvedValue({
      default: {
        pluginApiVersion: "1.0.0",
        moduleId: "example",
        version: "1.2.3",
        activate: (host: { react: unknown; api: { basePath: string } }) => {
          receivedReact = host.react;
          expect(host.api.basePath).toBe("/api/modules/example");
          return { components: { ExampleView } };
        },
      },
    });

    const loaded = await loadWebModule(manifest(), { importModule, loadStylesheet });

    expect(receivedReact).toBe(React);
    expect(loadStylesheet).toHaveBeenCalledWith(
      "/modules/example/assets/frontend/module.css",
      "example",
    );
    expect(importModule).toHaveBeenCalledWith(
      "/modules/example/assets/frontend/remoteEntry.js",
    );
    expect(loaded.activation.components.ExampleView).toBe(ExampleView);
    loaded.dispose();
    expect(disposeStyle).toHaveBeenCalledOnce();
  });

  it("rejects a legacy direct-component entry instead of accepting a possible second React", async () => {
    const importModule = vi.fn().mockResolvedValue({
      default: {
        pluginApiVersion: "1.0.0",
        moduleId: "example",
        version: "1.2.3",
        components: { ExampleView: () => null },
      },
    });
    const disposeStyle = vi.fn();

    await expect(loadWebModule(manifest(), {
      importModule,
      loadStylesheet: vi.fn().mockResolvedValue(disposeStyle),
    })).rejects.toThrow("activate(hostSdk)");
    expect(disposeStyle).toHaveBeenCalledOnce();
  });

  it("fails compatibility before executing or attaching module resources", async () => {
    const importModule = vi.fn();
    const loadStylesheet = vi.fn();
    const incompatible = manifest({
      compatibility: { core: ">=0.6.0, <0.7.0", platform_api: "^1.0.0", plugin_api: "^1.0.0" },
    });

    await expect(loadWebModule(incompatible, { importModule, loadStylesheet }))
      .rejects.toThrow("Core 0.5.0");
    expect(importModule).not.toHaveBeenCalled();
    expect(loadStylesheet).not.toHaveBeenCalled();
  });

  it("matches parameterized routes against Manifest-whitelisted components", async () => {
    const loaded = await loadWebModule(manifest({
      frontend: { ...manifest().frontend!, styles: [] },
    }), {
      loadStylesheet: vi.fn(),
      importModule: vi.fn().mockResolvedValue({
        default: {
          pluginApiVersion: "1.0.0",
          moduleId: "example",
          version: "1.2.3",
          activate: () => ({ components: { ExampleView: () => null } }),
        },
      }),
    });

    expect(matchModuleRoute(loaded, "/modules/example/hello%20world")?.params)
      .toEqual({ item: "hello world" });
    expect(matchModuleRoute(loaded, "/modules/another/item")).toBeNull();
  });
});

describe("catalog, paths and permissions", () => {
  it("isolates a malformed item while retaining valid module descriptors", () => {
    const invalid = {
      ...manifest(),
      id: "outside",
      frontend: { ...manifest().frontend, entry: "https://outside.example/plugin.js" },
    };
    const result = parseModuleCatalog([manifest(), invalid]);

    expect(result.modules.map((module) => module.id)).toEqual(["example"]);
    expect(result.issues).toEqual([{ moduleId: "outside", message: expect.any(String) }]);
  });

  it("keeps assets in a fixed same-origin module namespace", () => {
    expect(resolveModuleAsset("example", "frontend/remoteEntry.js", ".js"))
      .toBe("/modules/example/assets/frontend/remoteEntry.js");
    expect(() => resolveModuleAsset("example", "../outside.js", ".js")).toThrow();
    expect(() => resolveModuleAsset("example", "https://outside/plugin.js", ".js")).toThrow();
    expect(() => resolveModuleAsset("example", "frontend/%2e%2e/plugin.js", ".js")).toThrow();
  });

  it("keeps SDK API requests below the module gateway", () => {
    const api = createModuleApi("/api/modules/example");
    expect(api.basePath).toBe("/api/modules/example");
    expect(() => api.request("https://outside.example/")).toThrow();
    expect(() => api.request("/%2e%2e/platform/modules")).toThrow();
  });

  it("supports Manifest v1 semantic-version requirements", () => {
    expect(satisfiesSemverRange("0.5.0", ">=0.5.0, <0.6.0")).toBe(true);
    expect(satisfiesSemverRange("1.4.2", "^1.0.0")).toBe(true);
    expect(satisfiesSemverRange("2.0.0", "^1.0.0")).toBe(false);
    expect(satisfiesSemverRange("not-semver", "^1.0.0")).toBe(false);
  });

  it("shows public contributions and only explicitly granted protected contributions", () => {
    const canRead = createPermissionChecker(["example.read"]);
    expect(canRead(null)).toBe(true);
    expect(canRead("example.read")).toBe(true);
    expect(canRead("example.admin")).toBe(false);
    expect(createPermissionChecker(undefined)("example.read")).toBe(false);
    expect(createPermissionChecker(["*"])("example.admin")).toBe(true);
  });

  it("does not execute an enabled module frontend without access to any declared route", () => {
    expect(canLoadModuleFrontend(manifest(), [])).toBe(false);
    expect(canLoadModuleFrontend(manifest(), ["unrelated.read"])).toBe(false);
    expect(canLoadModuleFrontend(manifest(), ["example.read"])).toBe(true);
    expect(canLoadModuleFrontend(manifest(), ["*"])).toBe(true);
    expect(canLoadModuleFrontend(manifest({ enabled: false }), ["example.read"])).toBe(false);
  });
});
