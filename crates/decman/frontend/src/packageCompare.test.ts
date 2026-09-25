import { describe, expect, it } from "vitest";

import { compareUrl, participantLabel, partyParticipants } from "./packageCompare";
import type { DecentralizedParty } from "./types";

const ns = "a".repeat(68);
const self = `self::${ns}`;
const hostB = `host-b::${ns}`;
const hostC = `host-c::${ns}`;

const party: DecentralizedParty = {
  party_id: `alpha::${ns}`,
  threshold: 2,
  owners: [],
  participants: [self, hostB, hostC].map((participant_uid) => ({
    participant_uid,
    permission: "confirmation",
  })),
  contracts: [],
};

describe("compareUrl", () => {
  it("compares every configured peer when nothing is selected", () => {
    expect(compareUrl([])).toBe("/packages/compare-peers");
  });

  it("sends the selection as one encoded list", () => {
    const url = compareUrl([hostB, hostC]);
    const params = new URL(url, "http://x").searchParams;
    expect(params.get("participants")).toBe(`${hostB},${hostC}`);
  });
});

describe("partyParticipants", () => {
  it("lists the party's hosts without this node", () => {
    // This node's own packages are the reference column, so comparing it
    // with itself would only add a column of ticks.
    expect(partyParticipants(party, self)).toEqual([hostB, hostC]);
  });

  it("keeps every host when this node's id is not known yet", () => {
    expect(partyParticipants(party)).toEqual([self, hostB, hostC]);
  });
});

describe("participantLabel", () => {
  it("prefers the peer name", () => {
    expect(participantLabel({ id: hostB, name: "Operator B" })).toBe("Operator B");
  });

  it("falls back to the id prefix for a host with no peer row", () => {
    expect(participantLabel({ id: hostC, name: "" })).toBe("host-c");
  });
});
