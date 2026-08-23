import { describe, expect, it } from "vitest";
import type { AgentInstanceSummary, AgentInstanceStatus, MonitoringHistoryPoint } from "./types";
import {
  agentAuthorizationKeyGuidance,
  historyValues,
  latestHistoryValue,
  pendingAgentInstances,
} from "./MonitoringView";

function instance(requestId: string, status: AgentInstanceStatus): AgentInstanceSummary {
  return {
    request_id: requestId,
    instance_id: `host-${requestId}`,
    display_name: requestId,
    status,
    expires_at: "2026-08-15T12:00:00Z",
    created_at: "2026-08-15T11:45:00Z",
  };
}

describe("pending Agent instances", () => {
  it("only lists invitations that can still be activated", () => {
    const instances = [
      instance("pending", "pending"),
      instance("active", "active"),
      instance("expired", "expired"),
      instance("cancelled", "cancelled"),
    ];

    expect(pendingAgentInstances(instances).map(({ request_id }) => request_id))
      .toEqual(["pending"]);
    expect(instances).toHaveLength(4);
  });
});

describe("Agent authorization-key guidance", () => {
  it("describes only the current Windows and CLI pairing flows", () => {
    expect(agentAuthorizationKeyGuidance).toContain("Windows");
    expect(agentAuthorizationKeyGuidance).toContain("服务器地址");
    expect(agentAuthorizationKeyGuidance).toContain("授权密钥");
    expect(agentAuthorizationKeyGuidance).toContain("CLI");
    expect(agentAuthorizationKeyGuidance).toContain("激活页确认");
  });
});

describe("monitoring history", () => {
  const point = (value: number | null): MonitoringHistoryPoint => ({
    report_id: crypto.randomUUID(),
    collected_at: "2026-08-15T12:00:00Z",
    received_at: "2026-08-15T12:00:01Z",
    cpu_usage_percent: value,
    memory_usage_percent: null,
    network_received_bytes_per_second: null,
    network_transmitted_bytes_per_second: null,
    disk_read_bytes_per_second: null,
    disk_written_bytes_per_second: null,
    max_temperature_celsius: null,
    gpu_utilization_percent: null,
    gpu_memory_usage_percent: null,
  });

  it("keeps missing samples as holes instead of joining neighboring points", () => {
    const values = historyValues([point(20), point(null), point(40)], (item) => item.cpu_usage_percent);
    expect(values).toEqual([20, null, 40]);
  });

  it("shows a missing latest raw sample as unavailable rather than reusing an older value", () => {
    expect(latestHistoryValue([20, 30, null])).toBeNull();
    expect(latestHistoryValue([])).toBeNull();
  });
});
