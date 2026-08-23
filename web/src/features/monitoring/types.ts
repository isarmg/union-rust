// Telemetry values are read-only snapshots. Nullable values mean the agent could
// not collect the metric; the UI deliberately distinguishes those values from a real zero.
import type { JsonU64 } from "../../shared/lib/format";

export interface MonitoringCapability {
  name: string;
  available: boolean;
  source: string;
  error_kind: string | null;
  message: string | null;
}

export interface MonitoringHostSummary {
  id: string;
  /** Server-owned operator remark; the Agent does not report a device name. */
  name: string;
  os: string;
  os_version: string | null;
  kernel_version: string | null;
  arch: string;
  agent_version: string;
  registered_at: string;
  last_seen_at: string;
  latest_collected_at: string | null;
  status: "online" | "stale" | "offline";
  capabilities: MonitoringCapability[];
  cpu_usage_percent: number | null;
  memory_usage_percent: number | null;
  network_received_bytes_per_second: number | null;
  network_transmitted_bytes_per_second: number | null;
  disk_read_bytes_per_second: number | null;
  disk_written_bytes_per_second: number | null;
  max_temperature_celsius: number | null;
  gpu_utilization_percent: number | null;
  gpu_memory_usage_percent: number | null;
}

export interface MonitoringHostsResponse {
  hosts: MonitoringHostSummary[];
  total: number;
  limit: number;
  offset: number;
}

export interface MonitoringCpuReport {
  usage_percent: number;
  logical_count: number;
  physical_count: number | null;
  per_core_percent: number[];
}

export interface MonitoringMemoryReport {
  total_bytes: JsonU64;
  used_bytes: JsonU64;
  available_bytes: JsonU64;
  swap_total_bytes: JsonU64;
  swap_used_bytes: JsonU64;
}

export interface MonitoringNetworkReport {
  name: string;
  received_bytes_total: JsonU64;
  transmitted_bytes_total: JsonU64;
  received_bytes_per_second: number;
  transmitted_bytes_per_second: number;
  packets_received_total: JsonU64;
  packets_transmitted_total: JsonU64;
  receive_errors_total: JsonU64;
  transmit_errors_total: JsonU64;
}

export interface MonitoringDiskReport {
  name: string;
  mount_point: string;
  file_system: string;
  total_bytes: JsonU64;
  available_bytes: JsonU64;
  read_bytes_total: JsonU64;
  written_bytes_total: JsonU64;
  read_bytes_per_second: number;
  written_bytes_per_second: number;
  is_read_only: boolean;
}

export interface MonitoringTemperatureReport {
  id: string;
  label: string;
  celsius: number | null;
  max_celsius: number | null;
  critical_celsius: number | null;
  source: string;
}

export interface MonitoringGpuReport {
  id: string;
  vendor: string;
  name: string;
  utilization_percent: number | null;
  memory_total_bytes: JsonU64 | null;
  memory_used_bytes: JsonU64 | null;
  temperature_celsius: number | null;
  power_watts: number | null;
  core_clock_mhz: number | null;
  memory_clock_mhz: number | null;
  pcie_rx_bytes_per_second: number | null;
  pcie_tx_bytes_per_second: number | null;
  source: string;
}

export interface MonitoringAgentReport {
  schema_version: number;
  report_id: string;
  collected_at: string;
  host: {
    id: string;
    os: string;
    os_version: string | null;
    kernel_version: string | null;
    arch: string;
    agent_version: string;
  };
  interval_seconds: number;
  system: {
    uptime_seconds: JsonU64;
    cpu: MonitoringCpuReport;
    memory: MonitoringMemoryReport;
    networks: MonitoringNetworkReport[];
    disks: MonitoringDiskReport[];
    temperatures: MonitoringTemperatureReport[];
    gpus: MonitoringGpuReport[];
  };
  capabilities: MonitoringCapability[];
  agent: { spool_pending_batches: JsonU64; collector_errors: JsonU64 };
}

export type AgentInstanceStatus = "pending" | "expired" | "cancelled" | "active";

/** 管理端创建的单机 Agent 激活邀请；不包含任何 Agent 访问令牌。 */
export interface AgentInstanceSummary {
  request_id: string;
  instance_id: string;
  display_name: string;
  status: AgentInstanceStatus;
  expires_at: string;
  created_at: string;
}

/** 激活码是一次性凭据，仅在创建邀请的响应中返回。 */
export interface CreatedAgentInstance extends AgentInstanceSummary {
  activation_code: string;
}

export interface MonitoringHostDetailResponse {
  host: MonitoringHostSummary;
  latest: MonitoringAgentReport | null;
}

export interface MonitoringHistoryPoint {
  report_id: string;
  collected_at: string;
  received_at: string;
  cpu_usage_percent: number | null;
  memory_usage_percent: number | null;
  network_received_bytes_per_second: number | null;
  network_transmitted_bytes_per_second: number | null;
  disk_read_bytes_per_second: number | null;
  disk_written_bytes_per_second: number | null;
  max_temperature_celsius: number | null;
  gpu_utilization_percent: number | null;
  gpu_memory_usage_percent: number | null;
}

export interface MonitoringHistoryResponse {
  host_id: string;
  points: MonitoringHistoryPoint[];
}
