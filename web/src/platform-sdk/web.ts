import type * as ReactRuntime from "react";
import type { ComponentType } from "react";

/** Versions implemented by this Web Shell. Manifest ranges are checked against these values. */
export const CORE_VERSION = "0.6.0";
export const PLATFORM_API_VERSION = "1.0.0";
export const PLUGIN_API_VERSION = "2.0.0";

export interface ModuleRouteLocation {
  pathname: string;
  params: Readonly<Record<string, string>>;
}

export interface ModuleApi {
  readonly basePath: string;
  request<T>(path: string, init?: ModuleApiRequestInit): Promise<T>;
}

export interface ModuleApiRequestInit extends RequestInit {
  timeoutMs?: number;
  suppressAuthExpired?: boolean;
  expectedStatus?: number;
}

/** Props supplied by the Shell to every component exported by a module entry. */
export interface WebModuleComponentProps {
  api: ModuleApi;
  location: ModuleRouteLocation;
  navigate: (path: string, options?: { replace?: boolean }) => void;
  hasPermission: (permission: string) => boolean;
  actionRequest: number;
  onActionRequestHandled: (request: number) => void;
}

export interface WebModulePrimaryAction {
  component: string;
  label: string;
  /** Optional module-owned permission required before the Shell exposes the action. */
  permission?: string;
}

/**
 * The default export of a module's ESM entry.
 *
 * Route and menu metadata deliberately remain in the signed/validated Manifest. The JavaScript
 * entry activates against the Shell-owned React runtime and supplies component implementations
 * only, so executable code cannot silently add routes or permissions that Core did not register.
 */
export interface ActivatedWebModule {
  components: Readonly<Record<string, ComponentType<WebModuleComponentProps>>>;
  primaryActions?: readonly WebModulePrimaryAction[];
}

export interface WebModuleHostSdk {
  /** The only supported React instance. Remote entries must not bundle or import another copy. */
  react: typeof ReactRuntime;
  coreVersion: string;
  platformApiVersion: string;
  pluginApiVersion: string;
  module: Readonly<{ id: string; version: string; apiBase: string }>;
  api: ModuleApi;
}

export interface WebModuleEntry {
  pluginApiVersion: string;
  moduleId: string;
  version: string;
  activate(host: WebModuleHostSdk): ActivatedWebModule | Promise<ActivatedWebModule>;
}

export interface WebModuleEntryNamespace {
  default?: unknown;
}
