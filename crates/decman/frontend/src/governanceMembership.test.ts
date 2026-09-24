import { describe, expect, it } from "vitest";

import { getGovernanceMembership } from "./governanceMembership";
import type { GovernanceState } from "./types";

const nsA = "a".repeat(68);
const nsB = "b".repeat(68);

const state = (members: string[]): GovernanceState => ({
  contract_id: "00rules",
  governance_party: `gov::${nsA}`,
  members,
  threshold: 1,
  out_of_date: false,
});

describe("getGovernanceMembership", () => {
  it("reports a member when the full id is in the member set", () => {
    expect(
      getGovernanceMembership(`m1::${nsA}`, state([`m0::${nsA}`, `m1::${nsA}`])),
    ).toBe("member");
  });

  it("reports not a member when the id is absent", () => {
    expect(getGovernanceMembership(`m1::${nsA}`, state([`m0::${nsA}`]))).toBe(
      "not_member",
    );
  });

  it("reports unknown for an empty member set", () => {
    expect(getGovernanceMembership(`m1::${nsA}`, state([]))).toBe("unknown");
  });

  it("does not match the same prefix under another namespace", () => {
    expect(getGovernanceMembership(`m1::${nsA}`, state([`m1::${nsB}`]))).toBe(
      "not_member",
    );
  });

  it("does not match a prefix of the full id", () => {
    expect(
      getGovernanceMembership(`m1::${nsA}`, state([`m1::${nsA.slice(0, 16)}`])),
    ).toBe("not_member");
    expect(getGovernanceMembership(`m1::${nsA}`, state(["m1"]))).toBe(
      "not_member",
    );
  });

  it("reports unknown without governance state", () => {
    expect(getGovernanceMembership(`m1::${nsA}`, null)).toBe("unknown");
    expect(getGovernanceMembership(`m1::${nsA}`, undefined)).toBe("unknown");
  });

  it("reports unknown without a member party", () => {
    expect(getGovernanceMembership(undefined, state([`m1::${nsA}`]))).toBe(
      "unknown",
    );
  });
});
