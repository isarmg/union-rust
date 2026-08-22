import type { AgentPairingRequestSummary } from "./types";

export type AgentActivationRoute =
  | { isActivationRoute: false; requestId: null }
  | { isActivationRoute: true; requestId: string | null };

const ACTIVATION_PATH = "/agent/activate";
const CANONICAL_UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

/**
 * UnionC creates canonical UUID request identifiers. Treat every other path
 * segment as malformed.
 */
export function parseAgentActivationRoute(pathname: string): AgentActivationRoute {
  if (pathname === ACTIVATION_PATH || pathname === `${ACTIVATION_PATH}/`) {
    return { isActivationRoute: true, requestId: null };
  }
  if (!pathname.startsWith(`${ACTIVATION_PATH}/`)) {
    return { isActivationRoute: false, requestId: null };
  }

  const requestId = pathname.slice(ACTIVATION_PATH.length + 1);
  if (!CANONICAL_UUID.test(requestId)) {
    return { isActivationRoute: true, requestId: null };
  }
  return { isActivationRoute: true, requestId };
}

export function activationCodeForSubmission(value: string): string {
  return value.trim();
}

export function canActivatePairing(status: AgentPairingRequestSummary["status"]): boolean {
  return status === "waiting";
}
