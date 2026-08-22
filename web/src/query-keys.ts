export const queryKeys = {
  services: ["services"] as const,
  systemResources: ["system-resources"] as const,
  auth: { me: ["auth-me"] as const },
  monitoring: {
    hosts: ["monitoring-hosts"] as const,
    hostPage: (limit: number, offset: number) => ["monitoring-hosts", limit, offset] as const,
    host: (hostId: string) => ["monitoring-host", hostId] as const,
    history: (hostId: string) => ["monitoring-history", hostId] as const,
    agentInstances: ["monitoring-agent-instances"] as const,
  },
  agentActivation: {
    pairingRequest: (requestId: string) => ["agent-pairing-request", requestId] as const,
  },
  logs: { sunshine: (hostId: string) => ["logs", "sunshine", hostId] as const },
  sunshine: {
    hosts: ["sunshine-hosts"] as const,
    apps: (hostId: string) => ["sunshine-apps", hostId] as const,
    clients: (hostId: string) => ["sunshine-clients", hostId] as const,
    config: (hostId: string) => ["sunshine-config", hostId] as const,
  },
} as const;
