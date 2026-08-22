export const pathSegment = (value: string | number) => encodeURIComponent(String(value));
export const sunshineHostPath = (id: string) => `/api/services/sunshine/hosts/${pathSegment(id)}`;
export const monitoringHostPath = (id: string) => `/api/monitoring/hosts/${pathSegment(id)}`;
export const monitoringAgentInstancePath = (requestId: string) =>
  `/api/monitoring/agent-instances/${pathSegment(requestId)}`;
export const agentPairingRequestPath = (requestId: string) =>
  `/api/agent/v2/pairing-requests/${pathSegment(requestId)}`;
