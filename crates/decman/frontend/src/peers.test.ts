import { describe, expect, it } from "vitest";
import {
  heartbeatAge,
  parsePeerIdentity,
  peerIdentityString,
  peerSubtitle,
  NO_NODE_PARTY,
} from "./peers";

describe("peerIdentityString", () => {
  it("writes the three values in the exchange order", () => {
    expect(peerIdentityString("participant1::1220ab", "node1::1220cd", "Alice")).toBe(
      "participant1::1220ab,node1::1220cd,Alice",
    );
  });
});

describe("parsePeerIdentity", () => {
  it("round-trips an identity string", () => {
    const peer = parsePeerIdentity(
      peerIdentityString("participant1::1220ab", "node1::1220cd", "Alice"),
    );
    expect(peer).toEqual({
      participant_id: "participant1::1220ab",
      name: "Alice",
      party: "node1::1220cd",
    });
  });

  it("defaults a missing name to the participant id", () => {
    expect(parsePeerIdentity("participant1::1220ab,node1::1220cd")?.name).toBe(
      "participant1::1220ab",
    );
  });

  it("keeps a name that contains a comma", () => {
    expect(parsePeerIdentity("p::1,n::2,Acme, Inc")?.name).toBe("Acme, Inc");
  });

  it("refuses a string without a node party", () => {
    expect(parsePeerIdentity("participant1::1220ab")).toBeNull();
    expect(parsePeerIdentity("participant1::1220ab,,Alice")).toBeNull();
  });

  it("refuses an empty string", () => {
    expect(parsePeerIdentity("   ")).toBeNull();
  });
});

describe("peerSubtitle", () => {
  it("shows the node party", () => {
    expect(peerSubtitle({ participant_id: "p::1", name: "A", party: "n::2" })).toBe("n::2");
  });

  it("says so when the peer has no node party", () => {
    expect(peerSubtitle({ participant_id: "p::1", name: "A" })).toBe(NO_NODE_PARTY);
  });
});

describe("heartbeatAge", () => {
  it("reads as a time ago", () => {
    expect(heartbeatAge(undefined)).toBe("—");
    expect(heartbeatAge(null)).toBe("—");
    expect(heartbeatAge(-2)).toBe("now");
    expect(heartbeatAge(12)).toBe("12s ago");
    expect(heartbeatAge(3_000)).toBe("50m ago");
    expect(heartbeatAge(7_200)).toBe("2h ago");
    expect(heartbeatAge(200_000)).toBe("2d ago");
  });
});
