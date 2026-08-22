export const monitoringQueryKeys = {
  monitoring: {
    hosts: ["monitoring-hosts"] as const,
    hostPage: (limit: number, offset: number) => ["monitoring-hosts", limit, offset] as const,
    host: (hostId: string) => ["monitoring-host", hostId] as const,
    history: (hostId: string) => ["monitoring-history", hostId] as const,
    agentInstances: ["monitoring-agent-instances"] as const,
  },
} as const;
