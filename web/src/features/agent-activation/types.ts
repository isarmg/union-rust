/** Agent 在本机发起的浏览器配对完成后的公开、安全响应。 */
export interface AgentActivationResponse {
  instance_id: string;
  status: "active";
}

/** 浏览器激活页可读取的有限设备摘要；不得包含令牌或本机敏感信息。 */
export interface AgentPairingRequestSummary {
  request_id: string;
  os: string;
  arch: string;
  agent_version: string;
  status: "waiting" | "expired" | "denied" | "active";
  expires_at: string;
}
