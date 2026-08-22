import { ApiError, request } from "../../shared/api/client";
import { pathSegment } from "../../shared/api/paths";
import type { AgentActivationResponse, AgentPairingRequestSummary } from "./types";

export const agentActivationApi = {
  activateAgent: async (request_id: string, activation_code: string) => {
    try {
      return await request<AgentActivationResponse>("/api/agent/v2/activate", {
        method: "POST",
        body: JSON.stringify({ request_id, activation_code }),
        suppressAuthExpired: true,
      });
    } catch (error) {
      if (error instanceof ApiError && error.status === 401) {
        throw new ApiError("激活码无效或已过期", error.code, error.status);
      }
      throw error;
    }
  },
  agentPairingRequest: (requestId: string) => request<AgentPairingRequestSummary>(
    `/api/agent/v2/pairing-requests/${pathSegment(requestId)}`,
    { suppressAuthExpired: true },
  ),
};
