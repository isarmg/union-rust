export interface SunshineHostInfo {
  id: string;
  name: string;
  host: string;
  web_port: number;
  username: string;
  password_set: boolean;
  verify_tls: boolean;
  web_url: string;
  probe_status: "pending" | "complete";
  reachable: boolean | null;
  connected: boolean | null;
  connection_error?: string | null;
}

export interface SunshineHostSaveRequest {
  name: string;
  host: string;
  web_port: number;
  username: string;
  password?: string | null;
  verify_tls: boolean;
}

export type SunshineHostPatchRequest = Partial<SunshineHostSaveRequest>;

export interface SunshineApp {
  name: string;
  cmd?: string | null;
  index: number;
  "image-path"?: string | null;
  "working-dir"?: string;
  output?: string;
  "auto-detach"?: boolean;
  "wait-all"?: boolean;
  "exit-timeout"?: number;
  "prep-cmd"?: unknown[];
  detached?: unknown[];
  elevated?: boolean;
  "exclude-global-prep-cmd"?: boolean;
  [key: string]: unknown;
}

export interface SunshineAppsResponse { apps: SunshineApp[]; }
export interface SunshineClient { name?: string | null; uuid: string; enabled: boolean; }
export interface SunshineClientsResponse { status: boolean; named_certs: SunshineClient[]; }
export interface SunshineLogsResponse { content: string; }
export type SunshineConfig = Record<string, unknown>;
