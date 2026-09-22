import { describe, expect, it } from "vitest";

import { peerErrorTooltip } from "./PackagesPanel";
import type { PeerErrorKind, PeerPackageResult } from "../types";

const peer = (error_kind?: PeerErrorKind): PeerPackageResult => ({
  participant_id: "participant::1220ab",
  name: "operator-b",
  reachable: false,
  error_kind,
  packages: [],
});

describe("peerErrorTooltip", () => {
  it("blames this node for a failed topology read", () => {
    // A stale synchronizer-id cache here makes every peer look empty. The
    // operator has to know the fault is local, not with the peers.
    const text = peerErrorTooltip(peer("topology_read_failed"));
    expect(text).toContain("This node");
    expect(text).toContain("synchronizer");
  });

  it("blames the peer for an empty vetting set", () => {
    const text = peerErrorTooltip(peer("no_vetted_packages"));
    expect(text).toContain("This peer");
    expect(text).toContain("vetted no packages");
  });

  it("separates the two causes", () => {
    expect(peerErrorTooltip(peer("topology_read_failed"))).not.toEqual(
      peerErrorTooltip(peer("no_vetted_packages")),
    );
  });

  it("still says something when no kind is set", () => {
    expect(peerErrorTooltip(peer())).toBe("No package list for this peer");
  });

  it("names a Noise variant rather than dropping it", () => {
    // Nothing emits these now, but they stay on the wire. An old node talking
    // to a new UI must not render a blank tooltip.
    expect(peerErrorTooltip(peer("handshake_failed"))).toContain(
      "handshake_failed",
    );
  });
});
