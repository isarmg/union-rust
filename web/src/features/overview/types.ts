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

export interface SystemResources {
  cpu_usage_percent: number;
  memory_total_kib: number;
  memory_used_kib: number;
  network: NetworkThroughput;
  disk_throughput: DiskThroughput;
  disks: DiskInfo[];
}

export interface NetworkThroughput {
  received_bytes_per_second: number;
  transmitted_bytes_per_second: number;
  total_bytes_per_second: number;
}

export interface DiskThroughput {
  read_bytes_per_second: number;
  write_bytes_per_second: number;
  total_bytes_per_second: number;
}

export interface DiskInfo {
  name: string;
  mount_point: string;
  total_bytes: number;
  available_bytes: number;
}

