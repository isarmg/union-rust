export type ModuleHealthState =
  | "discovered"
  | "installing"
  | "starting"
  | "available"
  | "degraded"
  | "backoff"
  | "incompatible"
  | "stopped"
  | "failed";

export interface ModuleCompatibility {
  core: string;
  platform_api: string;
  plugin_api: string;
}

export interface ModuleDependency {
  id: string;
  version: string;
  optional: boolean;
}

export interface ModulePermissionDefinition {
  id: string;
  description: string;
}

export interface ModuleFrontendRoute {
  path: string;
  component: string;
  permission: string;
}

export interface ModuleFrontendMenuItem {
  id: string;
  label: string;
  route: string;
  permission: string;
  order: number;
}

export interface ModuleFrontend {
  entry: string;
  styles: string[];
  components: string[];
  api_base: string;
  routes: ModuleFrontendRoute[];
  menu: ModuleFrontendMenuItem[];
}

/** Runtime projection returned by GET /api/platform/modules. */
export interface PlatformModule {
  manifest_version: 1;
  id: string;
  display_name: string;
  description: string;
  version: string;
  compatibility: ModuleCompatibility;
  dependencies: ModuleDependency[];
  permissions: ModulePermissionDefinition[];
  frontend: ModuleFrontend;
  enabled: boolean;
  lifecycle_state: ModuleHealthState;
  health_message: string;
  pid: number | null;
  restart_count: number;
  checked_at: string | null;
  resolved_frontend: { entry: string; styles: string[] };
}

export interface CatalogIssue {
  moduleId: string;
  message: string;
}

export interface CatalogResult {
  modules: PlatformModule[];
  issues: CatalogIssue[];
}

const MODULE_ID = /^[a-z](?:[a-z0-9-]{0,62}[a-z0-9])?$/;
const COMPONENT_ID = /^[A-Z][A-Za-z0-9]{0,127}$/;
const PERMISSION_ID = /^[a-z](?:[a-z0-9-]{0,62}[a-z0-9])?(?:\.[a-z][a-z0-9-]*)+$/;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function requiredString(value: unknown, label: string): string {
  if (typeof value !== "string" || value.length === 0) throw new Error(`${label} must be a string`);
  return value;
}

function stringArray(value: unknown, label: string): string[] {
  if (!Array.isArray(value) || !value.every((item) => typeof item === "string")) {
    throw new Error(`${label} must be a string array`);
  }
  if (new Set(value).size !== value.length) throw new Error(`${label} contains duplicates`);
  return value;
}

function validateModulePath(moduleId: string, path: string, label: string): string {
  const prefix = `/modules/${moduleId}`;
  if ((!path.startsWith(`${prefix}/`) && path !== prefix) || path.includes("\\")
      || path.includes("?") || path.includes("#") || path.includes("//")) {
    throw new Error(`${label} must stay below ${prefix}`);
  }
  const segments = path.split("/").slice(3);
  if (segments.some((segment) => segment === "." || segment === ".." || segment.length === 0)) {
    throw new Error(`${label} contains an unsafe path segment`);
  }
  return path;
}

export function resolveModuleAsset(
  moduleId: string,
  relativePath: string,
  extension: ".js" | ".css",
): string {
  if (!MODULE_ID.test(moduleId)) throw new Error("module id is invalid");
  if (!relativePath.endsWith(extension) || relativePath.startsWith("/")
      || relativePath.includes("\\") || relativePath.includes("?") || relativePath.includes("#")) {
    throw new Error(`module asset must be a relative ${extension} path`);
  }
  const segments = relativePath.split("/");
  if (segments.some((segment) => !segment || segment === "." || segment === ".."
      || decodeURIComponent(segment) !== segment)) {
    throw new Error("module asset contains an unsafe path segment");
  }
  return `/modules/${moduleId}/assets/${relativePath}`;
}

function parseFrontend(moduleId: string, value: unknown): ModuleFrontend {
  if (!isRecord(value)) throw new Error("frontend must be an object");
  const entry = requiredString(value.entry, "frontend.entry");
  resolveModuleAsset(moduleId, entry, ".js");
  const styles = stringArray(value.styles, "frontend.styles");
  styles.forEach((style) => resolveModuleAsset(moduleId, style, ".css"));
  const components = stringArray(value.components, "frontend.components");
  if (!components.every((component) => COMPONENT_ID.test(component))) {
    throw new Error("frontend.components contains an invalid component name");
  }
  const apiBase = requiredString(value.api_base, "frontend.api_base");
  if (apiBase !== `/api/modules/${moduleId}`) {
    throw new Error(`frontend.api_base must be /api/modules/${moduleId}`);
  }
  if (!Array.isArray(value.routes)) throw new Error("frontend.routes must be an array");
  const routes = value.routes.map((routeValue, index): ModuleFrontendRoute => {
    if (!isRecord(routeValue)) throw new Error(`frontend.routes[${index}] must be an object`);
    const component = requiredString(routeValue.component, `frontend.routes[${index}].component`);
    if (!components.includes(component)) {
      throw new Error(`frontend.routes[${index}] references an undeclared component`);
    }
    return {
      path: validateModulePath(
        moduleId,
        requiredString(routeValue.path, `frontend.routes[${index}].path`),
        `frontend.routes[${index}].path`,
      ),
      component,
      permission: requiredString(routeValue.permission, `frontend.routes[${index}].permission`),
    };
  });
  if (!Array.isArray(value.menu)) throw new Error("frontend.menu must be an array");
  const menu = value.menu.map((menuValue, index): ModuleFrontendMenuItem => {
    if (!isRecord(menuValue)) throw new Error(`frontend.menu[${index}] must be an object`);
    const order = menuValue.order;
    if (typeof order !== "number" || !Number.isSafeInteger(order)) {
      throw new Error(`frontend.menu[${index}].order must be an integer`);
    }
    const route = validateModulePath(
      moduleId,
      requiredString(menuValue.route, `frontend.menu[${index}].route`),
      `frontend.menu[${index}].route`,
    );
    if (!routes.some((candidate) => candidate.path === route)) {
      throw new Error(`frontend.menu[${index}] references an undeclared route`);
    }
    return {
      id: requiredString(menuValue.id, `frontend.menu[${index}].id`),
      label: requiredString(menuValue.label, `frontend.menu[${index}].label`),
      route,
      permission: requiredString(menuValue.permission, `frontend.menu[${index}].permission`),
      order,
    };
  });
  if (new Set(menu.map((item) => item.id)).size !== menu.length) {
    throw new Error("frontend.menu contains duplicate ids");
  }
  return { entry, styles, components, api_base: apiBase, routes, menu };
}

function parsePlatformModule(value: unknown, index: number): PlatformModule {
  if (!isRecord(value)) throw new Error(`catalog item ${index} must be an object`);
  const id = requiredString(value.id, `catalog item ${index}.id`);
  if (!MODULE_ID.test(id)) throw new Error("module id is invalid");
  if (value.manifest_version !== 1) throw new Error("manifest_version is not supported");
  if (!isRecord(value.compatibility)) throw new Error("compatibility must be an object");
  const permissions = Array.isArray(value.permissions) ? value.permissions.map((item, permissionIndex) => {
    if (!isRecord(item)) throw new Error(`permissions[${permissionIndex}] must be an object`);
    const permissionId = requiredString(item.id, `permissions[${permissionIndex}].id`);
    if (!PERMISSION_ID.test(permissionId) || !permissionId.startsWith(`${id}.`)) {
      throw new Error(`permissions[${permissionIndex}].id is outside the module namespace`);
    }
    return {
      id: permissionId,
      description: requiredString(item.description, `permissions[${permissionIndex}].description`),
    };
  }) : (() => { throw new Error("permissions must be an array"); })();
  const frontend = parseFrontend(id, value.frontend);
  const permissionIds = new Set(permissions.map((permission) => permission.id));
  for (const contribution of [...frontend.routes, ...frontend.menu]) {
    if (!permissionIds.has(contribution.permission)) {
      throw new Error(`frontend references undeclared permission ${contribution.permission}`);
    }
  }
  return {
    manifest_version: 1,
    id,
    display_name: requiredString(value.display_name, "display_name"),
    description: requiredString(value.description, "description"),
    version: requiredString(value.version, "version"),
    compatibility: {
      core: requiredString(value.compatibility.core, "compatibility.core"),
      platform_api: requiredString(value.compatibility.platform_api, "compatibility.platform_api"),
      plugin_api: requiredString(value.compatibility.plugin_api, "compatibility.plugin_api"),
    },
    dependencies: Array.isArray(value.dependencies) ? value.dependencies.map((dependency, dependencyIndex) => {
      if (!isRecord(dependency)) throw new Error(`dependencies[${dependencyIndex}] must be an object`);
      return {
        id: requiredString(dependency.id, `dependencies[${dependencyIndex}].id`),
        version: requiredString(dependency.version, `dependencies[${dependencyIndex}].version`),
        optional: dependency.optional === true,
      };
    }) : (() => { throw new Error("dependencies must be an array"); })(),
    permissions,
    frontend,
    enabled: value.enabled === true,
    lifecycle_state: requiredString(value.lifecycle_state, "lifecycle_state") as ModuleHealthState,
    health_message: requiredString(value.health_message, "health_message"),
    pid: value.pid === null || typeof value.pid === "number" ? value.pid : null,
    restart_count: typeof value.restart_count === "number" ? value.restart_count : 0,
    checked_at: value.checked_at === null || typeof value.checked_at === "string"
      ? value.checked_at : null,
    resolved_frontend: (() => {
      if (!isRecord(value.resolved_frontend)) throw new Error("resolved_frontend must be an object");
      const expectedEntry = resolveModuleAsset(id, frontend.entry, ".js");
      const entry = requiredString(value.resolved_frontend.entry, "resolved_frontend.entry");
      const styles = stringArray(value.resolved_frontend.styles, "resolved_frontend.styles");
      const expectedStyles = frontend.styles.map((style) => resolveModuleAsset(id, style, ".css"));
      if (entry !== expectedEntry || styles.length !== expectedStyles.length
          || !styles.every((style, index) => style === expectedStyles[index])) {
        throw new Error("resolved_frontend does not match the validated Manifest assets");
      }
      return { entry, styles };
    })(),
  };
}

/** Invalid catalog items become isolated diagnostics instead of crashing the Shell. */
export function parseModuleCatalog(value: unknown): CatalogResult {
  if (!Array.isArray(value)) return { modules: [], issues: [{ moduleId: "catalog", message: "模块目录不是数组" }] };
  const modules: PlatformModule[] = [];
  const issues: CatalogIssue[] = [];
  value.forEach((item, index) => {
    try {
      modules.push(parsePlatformModule(item, index));
    } catch (error) {
      const moduleId = isRecord(item) && typeof item.id === "string" ? item.id : `#${index}`;
      issues.push({ moduleId, message: error instanceof Error ? error.message : "invalid manifest" });
    }
  });
  return { modules, issues };
}
