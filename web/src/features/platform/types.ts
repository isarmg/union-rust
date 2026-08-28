export type {
  ModuleCompatibility,
  ModuleDependency,
  ModuleFrontend,
  ModuleFrontendMenuItem,
  ModuleFrontendRoute,
  ModuleHealthState,
  ModulePermissionDefinition,
  PlatformModule,
} from "../../app/moduleCatalog";

export type JsonPrimitive = string | number | boolean | null;
export type JsonValue = JsonPrimitive | JsonValue[] | { [key: string]: JsonValue };

/** Runtime configuration projection returned by the Core configuration registry. */
export interface ModuleConfiguration {
  module: string;
  schema_version: number;
  schema: JsonValue;
  configured: boolean;
  /** Present when an older persisted value cannot be used with the bundled schema. */
  validation_error: string | null;
  /** Secrets are returned as the literal redaction marker `***`. */
  value: JsonValue | null;
}
