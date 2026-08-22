export const sunshineQueryKeys = {
  sunshine: {
    hosts: ["sunshine-hosts"] as const,
    apps: (hostId: string) => ["sunshine-apps", hostId] as const,
    clients: (hostId: string) => ["sunshine-clients", hostId] as const,
    config: (hostId: string) => ["sunshine-config", hostId] as const,
  },
  logs: { sunshine: (hostId: string) => ["logs", "sunshine", hostId] as const },
} as const;
