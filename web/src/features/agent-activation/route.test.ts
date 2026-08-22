import { describe, expect, it } from "vitest";
import {
  activationCodeForSubmission,
  canActivatePairing,
  parseAgentActivationRoute,
} from "./route";

describe("Agent activation route", () => {
  const requestId = "f84a2e90-a0d2-445b-907d-019a028bcabc";

  it("reads the canonical UUID request id emitted by UnionC", () => {
    expect(parseAgentActivationRoute(`/agent/activate/${requestId}`)).toEqual({
      isActivationRoute: true,
      requestId,
    });
  });

  it("keeps malformed activation URLs out of the authenticated application", () => {
    for (const pathname of [
      "/agent/activate",
      "/agent/activate/",
      "/agent/activate/request-123",
      "/agent/activate/F84A2E90-A0D2-445B-907D-019A028BCABC",
      `/agent/activate/${requestId}/`,
      "/agent/activate/a/b",
      "/agent/activate/%E0%A4%A",
    ]) {
      expect(parseAgentActivationRoute(pathname)).toEqual({
        isActivationRoute: true,
        requestId: null,
      });
    }
  });

  it("does not claim unrelated application paths", () => {
    expect(parseAgentActivationRoute("/monitoring")).toEqual({
      isActivationRoute: false,
      requestId: null,
    });
    expect(parseAgentActivationRoute("/agent/activated/example").isActivationRoute).toBe(false);
  });
});

describe("activation code submission", () => {
  it("trims copy and paste whitespace without changing the code", () => {
    expect(activationCodeForSubmission("  AbC-123\n")).toBe("AbC-123");
  });

  it("only enables a pairing request that is still waiting", () => {
    expect(canActivatePairing("waiting")).toBe(true);
    for (const status of ["expired", "denied", "active"] as const) {
      expect(canActivatePairing(status)).toBe(false);
    }
  });
});
