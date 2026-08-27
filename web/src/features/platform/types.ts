export type ModuleExecution = "in_process" | "service";
export type ModuleHealthState = "available" | "degraded" | "probing" | "unconfigured";

export type ModuleUi =
  | { kind: "embedded"; route: string }
  | { kind: "external"; public_url_env: string };

export interface PlatformModule {
  schema_version: number;
  id: string;
  display_name: string;
  description: string;
  version: string;
  execution: ModuleExecution;
  ui: ModuleUi;
  capabilities: string[];
  service: {
    base_url_env: string;
    liveness_path: string;
    readiness_path: string | null;
  } | null;
  database: Record<string, unknown>;
  configured: boolean;
  health: ModuleHealthState;
  health_message: string;
  launch_url: string | null;
  checked_at: string | null;
}
