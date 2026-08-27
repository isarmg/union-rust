export type ModuleExecution = "private_process";
export type ModuleHealthState =
  | "starting"
  | "probing"
  | "available"
  | "degraded"
  | "backoff"
  | "unconfigured"
  | "stopped";

export type ModuleUi =
  | { kind: "console"; route: string }
  | { kind: "gateway"; entry_path: string };

export interface ModuleService {
  binary: string;
  bind: string;
  gateway_prefix: string;
  liveness_path: string;
  readiness_path: string;
}

export type ModuleDatabase =
  | { profile: "postgres_schema"; database_env: string; schema: string; role: string }
  | { profile: "dedicated_postgres"; database_env: string; role: string }
  | { profile: "embedded_sqlite"; state_directory: string; rationale: string };

export interface PlatformModule {
  schema_version: number;
  id: string;
  display_name: string;
  description: string;
  version: string;
  execution: ModuleExecution;
  ui: ModuleUi;
  capabilities: string[];
  service: ModuleService;
  database: ModuleDatabase;
  health: ModuleHealthState;
  health_message: string;
  pid: number | null;
  restart_count: number;
  checked_at: string | null;
}
