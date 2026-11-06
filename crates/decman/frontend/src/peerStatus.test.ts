import { describe, expect, it } from "vitest";

import { peerStatusTooltip } from "./peerStatus";
import type { ParticipantStatus } from "./types";
import { WORKFLOW_STEPS } from "./workflowSteps";

const SELF = 0xd3;

const status = (over: Partial<ParticipantStatus> = {}): ParticipantStatus => ({
  id: `peer::${"a".repeat(68)}`,
  status: "HandshakeFailed",
  ...over,
});

describe("peerStatusTooltip", () => {
  it("tells the operator an older peer must upgrade", () => {
    expect(peerStatusTooltip(status({ wire_version: 0xd2 }), SELF)).toBe(
      "Protocol version mismatch: the peer runs an older decman protocol " +
        "(0xD2, this node 0xD3) — the peer must upgrade decman",
    );
  });

  it("tells the operator this node must upgrade for a newer peer", () => {
    expect(peerStatusTooltip(status({ wire_version: 0xd4 }), SELF)).toBe(
      "Protocol version mismatch: the peer runs a newer decman protocol " +
        "(0xD4, this node 0xD3) — upgrade this node",
    );
  });

  it("keeps the key hint when the peer reported no wire version", () => {
    expect(peerStatusTooltip(status(), SELF)).toBe(
      "Noise handshake failed - check if the public key is correct",
    );
  });

  it("keeps the key hint when the peer reports this node's own version", () => {
    expect(peerStatusTooltip(status({ wire_version: SELF }), SELF)).toBe(
      "Noise handshake failed - check if the public key is correct",
    );
  });

  it("names the mismatch without a side when this node's version is unknown", () => {
    expect(peerStatusTooltip(status({ wire_version: 0xd2 }), undefined)).toBe(
      "Protocol version mismatch: the peer runs decman protocol 0xD2, which this " +
        "node does not speak — one side must upgrade",
    );
  });

  it("leaves a connected peer's text alone", () => {
    expect(
      peerStatusTooltip(status({ status: "Connected", wire_version: SELF }), SELF),
    ).toBe("Connected via Noise protocol");
  });

  it("appends latency and the peer's workflow", () => {
    const steps = WORKFLOW_STEPS.Onboarding;
    const st = status({
      status: "Connected",
      latency_ms: 42,
      workflow: {
        kind: "Onboarding",
        role: "Peer",
        step: "SignDns",
        step_index: steps.findIndex((s) => s.name === "SignDns"),
        step_total: steps.length,
      },
    });

    expect(peerStatusTooltip(st, SELF)).toBe(
      "Connected via Noise protocol — 42 ms — in Onboarding (Signing the namespace)",
    );
  });
});
