// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import type { MonitoringHostSummary } from "../types";
import { MonitoringHostPanel } from "./HardwareDetails";

afterEach(cleanup);

const host: MonitoringHostSummary = {
  id: "host-one",
  name: "客厅主机",
  os: "windows",
  os_version: "11",
  kernel_version: null,
  arch: "x86_64",
  agent_version: "0.3.5",
  registered_at: "2026-08-21T12:00:00Z",
  last_seen_at: "2026-08-21T12:00:00Z",
  latest_collected_at: "2026-08-21T12:00:00Z",
  status: "online",
  capabilities: [],
  cpu_usage_percent: null,
  memory_usage_percent: null,
  network_received_bytes_per_second: null,
  network_transmitted_bytes_per_second: null,
  disk_read_bytes_per_second: null,
  disk_written_bytes_per_second: null,
  max_temperature_celsius: null,
  gpu_utilization_percent: null,
  gpu_memory_usage_percent: null,
};

it("associates monitoring tabs with stable panels and activates them from the keyboard", () => {
  render(
    <MonitoringHostPanel
      host={host}
      report={null}
      historyPoints={[]}
      detailLoading={false}
      detailError={null}
      historyLoading={false}
      historyError={null}
      onClose={() => undefined}
    />,
  );

  const tablist = screen.getByRole("tablist", { name: "客厅主机 详情分类" });
  const tabs = within(tablist).getAllByRole("tab");
  const panels = screen.getAllByRole("tabpanel", { hidden: true });
  expect(tabs).toHaveLength(7);
  expect(panels).toHaveLength(7);
  for (const tab of tabs) {
    const panel = document.getElementById(tab.getAttribute("aria-controls")!);
    expect(panel).not.toBeNull();
    expect(panel?.getAttribute("aria-labelledby")).toBe(tab.id);
  }

  tabs[0].focus();
  fireEvent.keyDown(tabs[0], { key: "ArrowLeft" });
  expect(document.activeElement).toBe(tabs[6]);
  expect(tabs[6].getAttribute("aria-selected")).toBe("true");
  fireEvent.keyDown(tabs[6], { key: "ArrowRight" });
  expect(document.activeElement).toBe(tabs[0]);
  fireEvent.keyDown(tabs[0], { key: "End" });
  expect(document.activeElement).toBe(tabs[6]);
  fireEvent.keyDown(tabs[6], { key: "Home" });
  expect(document.activeElement).toBe(tabs[0]);
  expect(tabs.map((tab) => tab.tabIndex)).toEqual([0, -1, -1, -1, -1, -1, -1]);
});
