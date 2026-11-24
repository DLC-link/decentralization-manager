import { describe, expect, it } from "vitest";

import {
  buildRevokeCouponReassignmentDelegation,
  buildSetupCouponReassignmentDelegation,
} from "./couponDelegationPayloads";

const fields = {
  proposalDelegationDso: " DSO::1 ",
  proposalDelegationAssigners: [" a::1 ", "", "b::1"],
  proposalDelegationSplit: [
    { beneficiary: " x::1 ", weight: "1" },
    { beneficiary: "y::1", weight: "1" },
    { beneficiary: "z::1", weight: "1" },
  ],
  proposalPriorDelegation: "",
};

describe("buildSetupCouponReassignmentDelegation", () => {
  it("sends the trimmed parties and the derived percentages", () => {
    expect(buildSetupCouponReassignmentDelegation(fields)).toEqual({
      type: "setup_coupon_reassignment_delegation",
      dso: "DSO::1",
      assigners: ["a::1", "b::1"],
      new_beneficiaries: [
        { beneficiary: "x::1", percentage: "0.3333333334" },
        { beneficiary: "y::1", percentage: "0.3333333333" },
        { beneficiary: "z::1", percentage: "0.3333333333" },
      ],
      prior_delegation: undefined,
    });
  });

  it("names the delegation it replaces", () => {
    expect(
      buildSetupCouponReassignmentDelegation({ ...fields, proposalPriorDelegation: " del-1 " }),
    ).toMatchObject({ prior_delegation: "del-1" });
  });

  it("needs at least one assigner", () => {
    expect(() =>
      buildSetupCouponReassignmentDelegation({ ...fields, proposalDelegationAssigners: [" ", ""] }),
    ).toThrow("At least one assigner is required");
  });

  it("refuses a repeated assigner after trimming", () => {
    expect(() =>
      buildSetupCouponReassignmentDelegation({
        ...fields,
        proposalDelegationAssigners: ["a::1", " a::1"],
      }),
    ).toThrow("Assigners must be unique");
  });

  it("refuses a split the ledger would reject", () => {
    expect(() =>
      buildSetupCouponReassignmentDelegation({ ...fields, proposalDelegationSplit: [] }),
    ).toThrow("Add at least one beneficiary");
    expect(() =>
      buildSetupCouponReassignmentDelegation({
        ...fields,
        proposalDelegationSplit: [{ beneficiary: "x::1", weight: "0.5" }],
      }),
    ).toThrow("Each weight must be a whole number greater than 0");
  });
});

describe("buildRevokeCouponReassignmentDelegation", () => {
  it("sends the trimmed delegation cid", () => {
    expect(buildRevokeCouponReassignmentDelegation({ proposalRevokeDelegationCid: " del-1 " })).toEqual({
      type: "revoke_coupon_reassignment_delegation",
      delegation: "del-1",
    });
  });
});
