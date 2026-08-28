import * as React from "react";
import type { ComponentType } from "react";
import { request, type ApiRequestInit } from "../shared/api/client";
import {
  CORE_VERSION,
  PLATFORM_API_VERSION,
  PLUGIN_API_VERSION,
  type ActivatedWebModule,
  type ModuleApi,
  type WebModuleComponentProps,
  type WebModuleEntry,
  type WebModuleEntryNamespace,
} from "../platform-sdk/web";
import {
  type ModuleFrontendRoute,
  type PlatformModule,
} from "./moduleCatalog";

interface Semver {
  major: number;
  minor: number;
  patch: number;
}

export interface LoadedWebModule {
  manifest: PlatformModule;
  entry: WebModuleEntry;
  activation: ActivatedWebModule;
  dispose: () => void;
}

export interface FailedWebModule {
  manifest: PlatformModule;
  error: Error;
}

export interface WebModuleLoadDependencies {
  importModule: (url: string) => Promise<WebModuleEntryNamespace>;
  loadStylesheet: (url: string, moduleId: string) => Promise<() => void>;
}

const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?$/;
const MODULE_RESOURCE_TIMEOUT_MS = 15_000;

async function withTimeout<T>(operation: Promise<T>, label: string): Promise<T> {
  let timeout: number | undefined;
  try {
    return await Promise.race([
      operation,
      new Promise<T>((_, reject) => {
        timeout = window.setTimeout(
          () => reject(new Error(`${label} 超过 ${MODULE_RESOURCE_TIMEOUT_MS / 1_000} 秒`)),
          MODULE_RESOURCE_TIMEOUT_MS,
        );
      }),
    ]);
  } finally {
    if (timeout !== undefined) window.clearTimeout(timeout);
  }
}

function parseSemver(value: string): Semver | null {
  const match = SEMVER.exec(value);
  if (!match) return null;
  return { major: Number(match[1]), minor: Number(match[2]), patch: Number(match[3]) };
}

function compareSemver(left: Semver, right: Semver): number {
  if (left.major !== right.major) return left.major - right.major;
  if (left.minor !== right.minor) return left.minor - right.minor;
  return left.patch - right.patch;
}

function upperBoundForCaret(version: Semver): Semver {
  if (version.major > 0) return { major: version.major + 1, minor: 0, patch: 0 };
  if (version.minor > 0) return { major: 0, minor: version.minor + 1, patch: 0 };
  return { major: 0, minor: 0, patch: version.patch + 1 };
}

/** Strict subset used by Manifest v1: exact, caret, and comma/space-separated comparators. */
export function satisfiesSemverRange(versionValue: string, rangeValue: string): boolean {
  const version = parseSemver(versionValue);
  if (!version || !rangeValue.trim()) return false;
  const range = rangeValue.trim();
  if (range.startsWith("^")) {
    const lower = parseSemver(range.slice(1));
    return Boolean(lower && compareSemver(version, lower) >= 0
      && compareSemver(version, upperBoundForCaret(lower)) < 0);
  }
  const exact = parseSemver(range);
  if (exact) return compareSemver(version, exact) === 0;
  const comparators = range.split(/[\s,]+/).filter(Boolean);
  if (!comparators.length) return false;
  return comparators.every((comparator) => {
    const match = /^(>=|<=|>|<|=)(.+)$/.exec(comparator);
    if (!match) return false;
    const target = parseSemver(match[2]);
    if (!target) return false;
    const comparison = compareSemver(version, target);
    switch (match[1]) {
      case ">=": return comparison >= 0;
      case "<=": return comparison <= 0;
      case ">": return comparison > 0;
      case "<": return comparison < 0;
      case "=": return comparison === 0;
      default: return false;
    }
  });
}

export function assertShellCompatibility(module: PlatformModule): void {
  const checks = [
    ["Core", CORE_VERSION, module.compatibility.core],
    ["Platform API", PLATFORM_API_VERSION, module.compatibility.platform_api],
    ["Plugin API", PLUGIN_API_VERSION, module.compatibility.plugin_api],
  ] as const;
  for (const [label, current, requirement] of checks) {
    if (!satisfiesSemverRange(current, requirement)) {
      throw new Error(`${label} ${current} 不满足模块要求 ${requirement}`);
    }
  }
}

function asEntry(namespace: WebModuleEntryNamespace, module: PlatformModule): WebModuleEntry {
  const candidate = namespace.default;
  if (typeof candidate !== "object" || candidate === null) {
    throw new Error("模块入口必须提供 default export");
  }
  const entry = candidate as Partial<WebModuleEntry>;
  if (entry.pluginApiVersion !== PLUGIN_API_VERSION) {
    throw new Error(`模块入口 Plugin API ${String(entry.pluginApiVersion)} 与 Shell 不兼容`);
  }
  if (entry.moduleId !== module.id) throw new Error("模块入口 moduleId 与 Manifest 不一致");
  if (entry.version !== module.version) throw new Error("模块入口 version 与 Manifest 不一致");
  if (typeof entry.activate !== "function") {
    throw new Error("模块入口必须通过 activate(hostSdk) 注册组件");
  }
  return entry as WebModuleEntry;
}

function validateActivation(activation: ActivatedWebModule, module: PlatformModule) {
  if (typeof activation !== "object" || activation === null
      || typeof activation.components !== "object" || activation.components === null) {
    throw new Error("模块 activate 返回值无效");
  }
  const componentNames = Object.keys(activation.components);
  const declared = module.frontend?.components ?? [];
  if (componentNames.length !== declared.length
      || !componentNames.every((name) => declared.includes(name))) {
    throw new Error("模块注册的 components 与 Manifest 声明不一致");
  }
  if (Object.values(activation.components).some((component) => typeof component !== "function")) {
    throw new Error("模块注册了不可渲染的 component");
  }
  if (activation.primaryActions !== undefined) {
    if (!Array.isArray(activation.primaryActions) || activation.primaryActions.some((action) => (
      !action || typeof action.component !== "string" || typeof action.label !== "string"
      || !declared.includes(action.component)
    ))) throw new Error("模块入口 primaryActions 无效");
  }
}

export async function loadWebModule(
  module: PlatformModule,
  dependencies: WebModuleLoadDependencies = browserLoadDependencies,
): Promise<LoadedWebModule> {
  if (!module.enabled || module.frontend === null) throw new Error("模块未启用 Web 入口");
  assertShellCompatibility(module);
  const disposeStyles: Array<() => void> = [];
  try {
    const stylesheetUrls = module.resolved_frontend.styles;
    for (const stylesheetUrl of stylesheetUrls) {
      disposeStyles.push(await dependencies.loadStylesheet(stylesheetUrl, module.id));
    }
    const entry = asEntry(
      await withTimeout(
        dependencies.importModule(module.resolved_frontend.entry),
        `模块 ${module.id} 入口加载`,
      ),
      module,
    );
    const activation = await entry.activate({
      react: React,
      coreVersion: CORE_VERSION,
      platformApiVersion: PLATFORM_API_VERSION,
      pluginApiVersion: PLUGIN_API_VERSION,
      module: { id: module.id, version: module.version, apiBase: module.frontend.api_base },
      api: createModuleApi(module.frontend.api_base),
    });
    validateActivation(activation, module);
    return {
      manifest: module,
      entry,
      activation,
      dispose: () => { disposeStyles.splice(0).forEach((dispose) => dispose()); },
    };
  } catch (error) {
    disposeStyles.splice(0).forEach((dispose) => dispose());
    throw error;
  }
}

function importBrowserModule(url: string): Promise<WebModuleEntryNamespace> {
  return import(/* @vite-ignore */ url) as Promise<WebModuleEntryNamespace>;
}

function loadBrowserStylesheet(url: string, moduleId: string): Promise<() => void> {
  return new Promise((resolve, reject) => {
    const link = document.createElement("link");
    const timeout = window.setTimeout(() => {
      link.remove();
      reject(new Error(`模块 ${moduleId} 样式加载超过 ${MODULE_RESOURCE_TIMEOUT_MS / 1_000} 秒`));
    }, MODULE_RESOURCE_TIMEOUT_MS);
    link.rel = "stylesheet";
    link.href = url;
    link.dataset.unionModule = moduleId;
    link.addEventListener("load", () => {
      window.clearTimeout(timeout);
      resolve(() => link.remove());
    }, { once: true });
    link.addEventListener("error", () => {
      window.clearTimeout(timeout);
      link.remove();
      reject(new Error(`模块样式加载失败：${url}`));
    }, { once: true });
    document.head.append(link);
  });
}

export const browserLoadDependencies: WebModuleLoadDependencies = {
  importModule: importBrowserModule,
  loadStylesheet: loadBrowserStylesheet,
};

/** Test seam only; production always retains the default same-origin loader. */
export const moduleRuntimeEnvironment = { load: loadWebModule };

export function createPermissionChecker(permissions: readonly string[] | undefined) {
  const granted = new Set(permissions ?? []);
  return (permission: string | null): boolean => permission === null
    || granted.has("*") || granted.has(permission);
}

function safeApiPath(path: string): string {
  if (!path.startsWith("/") || path.startsWith("//") || path.includes("\\") || path.includes("#")) {
    throw new Error("模块 API 路径必须是单个同源绝对路径片段");
  }
  const pathname = path.split("?", 1)[0];
  let segments: string[];
  try { segments = pathname.split("/").map(decodeURIComponent); } catch {
    throw new Error("模块 API 路径编码无效");
  }
  if (segments.some((segment) => segment === "." || segment === "..")) {
    throw new Error("模块 API 路径不能包含目录跳转");
  }
  return path;
}

export function createModuleApi(basePath: string): ModuleApi {
  return {
    basePath,
    request: <T>(path: string, init?: RequestInit) => request<T>(
      `${basePath}${safeApiPath(path)}`,
      init as ApiRequestInit | undefined,
    ),
  };
}

export interface MatchedModuleRoute {
  route: ModuleFrontendRoute;
  params: Record<string, string>;
  component: ComponentType<WebModuleComponentProps>;
}

export function matchModuleRoute(module: LoadedWebModule, pathname: string): MatchedModuleRoute | null {
  for (const route of module.manifest.frontend?.routes ?? []) {
    const params = matchPath(route.path, pathname);
    if (params) return { route, params, component: module.activation.components[route.component] };
  }
  return null;
}

export function matchPath(pattern: string, pathname: string): Record<string, string> | null {
  const patternSegments = pattern.split("/").filter(Boolean);
  const pathSegments = pathname.split("/").filter(Boolean);
  const params: Record<string, string> = {};
  for (let index = 0; index < patternSegments.length; index += 1) {
    const expected = patternSegments[index];
    if (expected === "*") {
      try {
        params["*"] = pathSegments.slice(index).map(decodeURIComponent).join("/");
      } catch {
        return null;
      }
      return params;
    }
    const actual = pathSegments[index];
    if (actual === undefined) return null;
    if (expected.startsWith(":")) {
      try { params[expected.slice(1)] = decodeURIComponent(actual); } catch { return null; }
    }
    else if (expected !== actual) return null;
  }
  return patternSegments.length === pathSegments.length ? params : null;
}
