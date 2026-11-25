import { describe, expect, it } from "vitest";

import { peerStatusTooltip, peerVersionLabel } from "./peerStatus";
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

  it("keeps the text of every other status", () => {
    expect(
      peerStatusTooltip(status({ status: "CurrentNode", wire_version: SELF }), SELF),
    ).toBe("This is the current node");
    expect(peerStatusTooltip(status({ status: "Unreachable" }), SELF)).toBe(
      "Cannot reach peer (TCP connection failed)",
    );
    expect(peerStatusTooltip(undefined, SELF)).toBe("Status unknown");
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

describe("peerVersionLabel", () => {
  it("shows a release by its tag", () => {
    expect(peerVersionLabel("1.15.0", "v1.15.0")).toEqual({ version: "v1.15.0" });
  });

  it("shows a per-commit build as its semver plus the commit", () => {
    expect(peerVersionLabel("1.15.0", "abc1234")).toEqual({ version: "v1.15.0", build: "abc1234" });
  });

  it("shows a local build as it names itself", () => {
    expect(peerVersionLabel("1.15.0", "1.15.0-dev")).toEqual({ version: "1.15.0-dev" });
  });

  it("falls back to the semver for a peer that predates build_version", () => {
    expect(peerVersionLabel("1.13.0", undefined)).toEqual({ version: "v1.13.0" });
  });

  it("shows a dash when the peer reported nothing", () => {
    expect(peerVersionLabel(undefined, undefined)).toEqual({ version: "—" });
  });
});
