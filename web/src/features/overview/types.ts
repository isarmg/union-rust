export interface ServiceStatus {
  name: string;
  kind: string;
  runtime_state: string;
  healthy: boolean;
  address: string | null;
  pid: number | null;
  message: string;
  updated_at: string;
}
