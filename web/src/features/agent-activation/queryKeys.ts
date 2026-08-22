export const agentActivationQueryKeys = {
  agentActivation: {
    pairingRequest: (requestId: string) => ["agent-pairing-request", requestId] as const,
  },
} as const;
