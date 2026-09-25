import { describe, expect, it } from "vitest";

import {
  PACKAGE_GROUPS,
  compareCell,
  compareUrl,
  filterTerms,
  indexPeer,
  matchesPackageFilter,
  participantLabel,
  partyParticipants,
  rowDiffers,
  summarize,
} from "./packageCompare";
import type { DecentralizedParty, PackageInfo, PeerPackageResult } from "./types";

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

describe("filterTerms", () => {
  it("splits a pasted list on commas, spaces and new lines", () => {
    expect(filterTerms("utility-registry-v0, utility-credential-v0\nsplice-amulet")).toEqual([
      "utility-registry-v0",
      "utility-credential-v0",
      "splice-amulet",
    ]);
  });

  it("drops a .dar suffix, since operators paste file names", () => {
    expect(filterTerms("Utility-Registry-V0.dar")).toEqual(["utility-registry-v0"]);
  });

  it("gives no terms for blank input", () => {
    expect(filterTerms("  ,\n ")).toEqual([]);
  });
});

describe("matchesPackageFilter", () => {
  const registry = { name: "utility-registry-app-v0", package_id: "aa11" };
  const credential = { name: "utility-credential-v0", package_id: "bb22" };
  const cbtc = { name: "cbtc", package_id: "cc33" };
  const group = (key: string) => PACKAGE_GROUPS.filter((g) => g.key === key);

  it("keeps every package when nothing is set", () => {
    expect(matchesPackageFilter(cbtc, [], [])).toBe(true);
  });

  it("keeps only the group's packages", () => {
    expect(matchesPackageFilter(registry, group("registry"), [])).toBe(true);
    expect(matchesPackageFilter(credential, group("registry"), [])).toBe(false);
    expect(matchesPackageFilter(credential, group("utility"), [])).toBe(true);
  });

  it("keeps a package that matches any one term, by name or id", () => {
    expect(matchesPackageFilter(cbtc, [], ["nope", "cbtc"])).toBe(true);
    expect(matchesPackageFilter(cbtc, [], ["cc3"])).toBe(true);
    expect(matchesPackageFilter(cbtc, [], ["nope"])).toBe(false);
  });

  it("joins a group and terms with or", () => {
    expect(matchesPackageFilter(cbtc, group("registry"), ["cbtc"])).toBe(true);
    expect(matchesPackageFilter(registry, group("registry"), ["cbtc"])).toBe(true);
  });
});

describe("compareCell", () => {
  const local: PackageInfo = { package_id: "reg-040", name: "utility-registry", version: "0.4.0" };
  const peer = (
    packages: PackageInfo[],
    reachable = true,
  ): PeerPackageResult => ({ participant_id: hostB, name: "b", reachable, packages });

  it("matches the same package id", () => {
    expect(compareCell(indexPeer(peer([local])), local).status).toBe("match");
  });

  it("matches the same name and version under another id", () => {
    const rebuilt = { ...local, package_id: "reg-040-rebuilt" };
    expect(compareCell(indexPeer(peer([rebuilt])), local).status).toBe("match");
  });

  it("names the other versions a participant vets", () => {
    const older = { ...local, package_id: "reg-030", version: "0.3.0" };
    const cell = compareCell(indexPeer(peer([older])), local);
    expect(cell).toEqual({ status: "other_version", versions: ["0.3.0"] });
  });

  it("reports a package the participant does not vet at all as missing", () => {
    const other = { package_id: "x", name: "cbtc", version: "1.0.0" };
    expect(compareCell(indexPeer(peer([other])), local).status).toBe("missing");
  });

  it("does not call a participant with no package list a difference", () => {
    expect(compareCell(indexPeer(peer([], false)), local).status).toBe("unreachable");
  });

  it("cannot compare a local package with no name beyond its id", () => {
    const nameless = { package_id: "zz", name: "", version: "" };
    expect(compareCell(indexPeer(peer([local])), nameless).status).toBe("unknown");
  });

  it("counts vetted ids this node has no name for", () => {
    const unnamed = { package_id: "q", name: "", version: "" };
    expect(indexPeer(peer([local, unnamed])).unnamed).toBe(1);
  });
});

describe("summarize", () => {
  const a: PackageInfo = { package_id: "a1", name: "a", version: "1" };
  const b: PackageInfo = { package_id: "b1", name: "b", version: "1" };
  const reachable: PeerPackageResult = {
    participant_id: hostB,
    name: "b",
    reachable: true,
    packages: [a, { package_id: "b0", name: "b", version: "0" }],
  };
  const down: PeerPackageResult = {
    participant_id: hostC,
    name: "c",
    reachable: false,
    packages: [],
  };

  it("counts differences only at reachable participants", () => {
    const peers = [indexPeer(reachable), indexPeer(down)];
    expect(summarize([a, b], peers)).toEqual({
      packages: 2,
      differing: 1,
      missing: 0,
      unavailable: 1,
    });
    expect(rowDiffers(a, peers)).toBe(false);
    expect(rowDiffers(b, peers)).toBe(true);
  });
});
