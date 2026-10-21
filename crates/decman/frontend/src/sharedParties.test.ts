import { describe, expect, it } from "vitest";

import {
  participantName,
  partiesSharedWith,
  sharedPartyCounts,
} from "./sharedParties";
import type { DecentralizedParty, Peer } from "./types";

const ns = "a".repeat(68);
const self = `self::${ns}`;
const bravo = `bravo::${ns}`;
const charlie = `charlie::${ns}`;

const party = (prefix: string, hosts: string[]): DecentralizedParty => ({
  party_id: `${prefix}::${ns}`,
  threshold: 1,
  owners: [],
  participants: hosts.map((participant_uid) => ({
    participant_uid,
    permission: "submission",
  })),
  contracts: [],
});

const parties = [
  party("alpha", [self, bravo, charlie]),
  party("beta", [self, bravo]),
  party("gamma", [self, charlie]),
];

describe("partiesSharedWith", () => {
  it("keeps only the parties the participant hosts", () => {
    expect(partiesSharedWith(parties, bravo).map((p) => p.party_id)).toEqual([
      `alpha::${ns}`,
      `beta::${ns}`,
    ]);
  });

  it("is empty for a participant that hosts none of them", () => {
    expect(partiesSharedWith(parties, `delta::${ns}`)).toEqual([]);
  });
});

describe("sharedPartyCounts", () => {
  it("counts the parties each participant hosts", () => {
    const counts = sharedPartyCounts(parties);

    expect(counts.get(self)).toBe(3);
    expect(counts.get(bravo)).toBe(2);
    expect(counts.get(charlie)).toBe(2);
    expect(counts.has(`delta::${ns}`)).toBe(false);
  });
});

describe("participantName", () => {
  const peers: Peer[] = [
    { participant_id: bravo, name: "Bravo", address: "b", port: 1, public_key: "" },
    { participant_id: charlie, name: "", address: "c", port: 1, public_key: "" },
  ];

  it("uses the peer name, else the id's prefix", () => {
    expect(participantName(bravo, peers)).toBe("Bravo");
    expect(participantName(charlie, peers)).toBe("charlie");
    expect(participantName(`delta::${ns}`, peers)).toBe("delta");
    expect(participantName(bravo, null)).toBe("bravo");
  });
});
