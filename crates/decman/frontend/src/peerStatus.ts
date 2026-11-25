import type { ParticipantStatus } from "./types";
import { currentStepLabel, workflowKindLabel } from "./workflowSteps";

const HANDSHAKE_FAILED = "Noise handshake failed - check if the public key is correct";

/** A wire version byte, e.g. `0xD3`. */
const hexByte = (v: number): string => `0x${v.toString(16).toUpperCase().padStart(2, "0")}`;

/**
 * A peer that refused our frame on another wire version reports its byte.
 * Wire versions only increase, so the lower byte is the older build.
 */
const handshakeFailedText = (peer: number | undefined, self: number | undefined): string => {
  if (peer == null || peer === self) return HANDSHAKE_FAILED;
  if (self == null) {
    return (
      `Protocol version mismatch: the peer runs decman protocol ${hexByte(peer)}, ` +
      "which this node does not speak — one side must upgrade"
    );
  }
  const versions = `${hexByte(peer)}, this node ${hexByte(self)}`;
  return peer < self
    ? `Protocol version mismatch: the peer runs an older decman protocol (${versions}) — ` +
        "the peer must upgrade decman"
    : `Protocol version mismatch: the peer runs a newer decman protocol (${versions}) — ` +
        "upgrade this node";
};

const statusText = (
  st: ParticipantStatus | undefined,
  selfWireVersion: number | undefined,
): string => {
  switch (st?.status) {
    case "Connected":
      return "Connected via Noise protocol";
    case "CurrentNode":
      return "This is the current node";
    case "Unreachable":
      return "Cannot reach peer (TCP connection failed)";
    case "HandshakeFailed":
      return handshakeFailedText(st.wire_version, selfWireVersion);
    default:
      return "Status unknown";
  }
};

/**
 * A peer's status dot tooltip, with its round-trip latency and active
 * workflow. `selfWireVersion` is the current node's wire version, which
 * decides whether a refusing peer is the older or the newer side.
 */
export const peerStatusTooltip = (
  st: ParticipantStatus | undefined,
  selfWireVersion: number | undefined,
): string => {
  let title = statusText(st, selfWireVersion);
  if (st?.latency_ms != null) title += ` — ${st.latency_ms} ms`;
  if (st?.workflow) {
    const w = st.workflow;
    const step = currentStepLabel({ ...w, current_step: w.step });
    title += ` — in ${workflowKindLabel(w.kind)} (${step})`;
  }
  return title;
};

/**
 * A node's version as the compatibility semver that peers gate on, so every
 * row of the peers table compares on the same scale. A build whose identity
 * is not that semver, such as a per-commit image, adds its build after it.
 */
export const peerVersionLabel = (
  version: string | undefined,
  build: string | undefined,
): { version: string; build?: string } => {
  if (!version) return { version: build ?? "—" };
  if (!build || build.includes(version)) return { version: build ?? `v${version}` };
  return { version: `v${version}`, build };
};
