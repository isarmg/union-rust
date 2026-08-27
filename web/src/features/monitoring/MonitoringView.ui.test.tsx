// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { monitoringApi as api } from "./api";
import { MonitoringView } from "./MonitoringView";
import { monitoringQueryKeys as queryKeys } from "./queryKeys";
import type {
  MonitoringAgentReport,
  MonitoringHistoryPoint,
  MonitoringHostSummary,
} from "./types";

const host: MonitoringHostSummary = {
  id: "host-1",
  name: "书房主机",
  os: "windows",
  os_version: "11",
  kernel_version: "10.0.26100",
  arch: "x86_64",
  agent_version: "0.4.0",
  registered_at: "2026-08-23T12:00:00Z",
  last_seen_at: "2026-08-23T12:10:00Z",
  latest_collected_at: "2026-08-23T12:10:00Z",
  status: "online",
  capabilities: [],
  cpu_usage_percent: 25,
  memory_usage_percent: 50,
  network_received_bytes_per_second: 1024,
  network_transmitted_bytes_per_second: 512,
  disk_read_bytes_per_second: 2048,
  disk_written_bytes_per_second: 4096,
  max_temperature_celsius: 63,
  gpu_utilization_percent: 40,
  gpu_memory_usage_percent: 30,
};

const reportedCapabilities: MonitoringAgentReport["capabilities"] = [{
    name: "gpu.nvidia",
    available: true,
    source: "nvidia-smi",
    error_kind: null,
    message: null,
}];

const report: MonitoringAgentReport = {
  schema_version: 1,
  report_id: "report-1",
  collected_at: "2026-08-23T12:10:00Z",
  host: {
    id: host.id,
    os: host.os,
    os_version: host.os_version,
    kernel_version: host.kernel_version,
    arch: host.arch,
    agent_version: host.agent_version,
  },
  interval_seconds: 10,
  system: {
    uptime_seconds: 3600,
    cpu: { usage_percent: 25, logical_count: 16, physical_count: 8, per_core_percent: [] },
    memory: {
      total_bytes: 32_000_000_000,
      used_bytes: 16_000_000_000,
      available_bytes: 16_000_000_000,
      swap_total_bytes: 0,
      swap_used_bytes: 0,
    },
    networks: [{
      name: "Ethernet",
      received_bytes_total: 1000,
      transmitted_bytes_total: 2000,
      received_bytes_per_second: 1024,
      transmitted_bytes_per_second: 512,
      packets_received_total: 10,
      packets_transmitted_total: 20,
      receive_errors_total: 0,
      transmit_errors_total: 0,
    }],
    disks: [{
      name: "C:",
      mount_point: "C:/",
      file_system: "NTFS",
      total_bytes: 1_000_000,
      available_bytes: 400_000,
      read_bytes_total: 1000,
      written_bytes_total: 2000,
      read_bytes_per_second: 2048,
      written_bytes_per_second: 4096,
      is_read_only: false,
    }],
    temperatures: [{
      id: "cpu-package",
      label: "CPU Package",
      celsius: 63,
      max_celsius: 90,
      critical_celsius: 100,
      source: "LibreHardwareMonitor",
    }],
    gpus: [{
      id: "gpu-0",
      vendor: "NVIDIA",
      name: "RTX Test",
      utilization_percent: 40,
      memory_total_bytes: 8_000_000_000,
      memory_used_bytes: 2_400_000_000,
      temperature_celsius: 55,
      power_watts: 100,
      core_clock_mhz: 1800,
      memory_clock_mhz: 7000,
      pcie_rx_bytes_per_second: 100,
      pcie_tx_bytes_per_second: 200,
      source: "nvidia-smi",
    }],
  },
  capabilities: reportedCapabilities,
  agent: { spool_pending_batches: 0, collector_errors: 0 },
};

const historyPoint: MonitoringHistoryPoint = {
  report_id: report.report_id,
  collected_at: report.collected_at,
  received_at: "2026-08-23T12:10:01Z",
  cpu_usage_percent: 25,
  memory_usage_percent: 50,
  network_received_bytes_per_second: 1024,
  network_transmitted_bytes_per_second: 512,
  disk_read_bytes_per_second: 2048,
  disk_written_bytes_per_second: 4096,
  max_temperature_celsius: 63,
  gpu_utilization_percent: 40,
  gpu_memory_usage_percent: 30,
};

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function renderView() {
  vi.spyOn(api, "monitoringAgentInstances").mockResolvedValue([]);
  vi.spyOn(api, "monitoringHosts").mockResolvedValue({
    hosts: [host], total: 1, limit: 20, offset: 0,
  });
  vi.spyOn(api, "monitoringHost").mockResolvedValue({ host, latest: report });
  vi.spyOn(api, "monitoringHistory").mockResolvedValue({
    host_id: host.id,
    points: [historyPoint],
  });
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  });
  queryClient.setQueryData(queryKeys.monitoring.hostPage(20, 0), {
    hosts: [host], total: 1, limit: 20, offset: 0,
  });
  const rendered = render(
    <QueryClientProvider client={queryClient}>
      <MonitoringView />
    </QueryClientProvider>,
  );
  return { ...rendered, queryClient };
}

describe("monitoring host detail panel", () => {
  it("opens beside the card, presents categories as tables, and keeps the history cards", async () => {
    const user = userEvent.setup();
    const { container } = renderView();
    const card = screen.getByRole("article", { name: /书房主机/ });

    await user.click(card);
    expect(screen.queryByRole("dialog")).toBeNull();
    const detailTrigger = within(card).getByRole("button", { name: "详情" });
    await user.click(detailTrigger);

    const dialog = await screen.findByRole("dialog", { name: "书房主机 详情面板" });
    expect(dialog.closest(".monitoring-master-detail")).toBeTruthy();
    expect(within(dialog).queryByText("书房主机 详情")).toBeNull();
    const closeButton = within(dialog).getByRole("button", { name: "关闭详情面板" });
    expect(closeButton.closest(".sunshine-panel-nav-row")).toBeTruthy();
    expect(document.activeElement).toBe(closeButton);
    expect(within(dialog).getByRole("table", { name: "实例信息" })).toBeTruthy();
    expect(within(dialog).getByRole("table", { name: "实时指标" })).toBeTruthy();

    for (const [tab, table] of [
      ["网络", "网络接口"],
      ["磁盘", "磁盘与文件系统"],
      ["GPU", "GPU"],
      ["温度", "温度传感器"],
      ["能力", "采集能力"],
    ]) {
      await user.click(within(dialog).getByRole("tab", { name: tab }));
      expect(within(dialog).getByRole("table", { name: table })).toBeTruthy();
    }
    expect(within(dialog).getByText("gpu.nvidia")).toBeTruthy();

    await user.click(within(dialog).getByRole("tab", { name: "历史" }));
    await waitFor(() => expect(api.monitoringHistory).toHaveBeenCalledWith(host.id));
    expect(container.querySelector(".monitoring-history-panel .sarmg-grid.metric-grid")).toBeTruthy();
    expect(container.querySelectorAll(".monitoring-history-panel article.sarmg-card.metric")).toHaveLength(6);

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog", { name: "书房主机 详情面板" })).toBeNull();
    await waitFor(() => expect(document.activeElement).toBe(detailTrigger));
  });
});
