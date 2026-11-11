import { describe, expect, it } from "vitest";

import {
  buildAcceptExternalPartySetup,
  buildSetupMintingDelegation,
} from "./mintingDelegationPayloads";

const fields = {
  proposalExpectedDso: "DSO::1",
  proposalDelegate: "validator::1",
  proposalDelegationExpiresAt: "2026-12-31T23:00",
  proposalAmuletMergeLimit: "10",
  proposalDescription: "Collect rewards",
};

describe("buildSetupMintingDelegation", () => {
  it("sends the expiry as micros since epoch and the merge limit as a number", () => {
    expect(buildSetupMintingDelegation(fields)).toEqual({
      type: "setup_minting_delegation",
      delegate: "validator::1",
      dso: "DSO::1",
      expires_at_micros: new Date("2026-12-31T23:00").getTime() * 1000,
      amulet_merge_limit: 10,
      description: "Collect rewards",
    });
  });

  it("refuses a missing or unreadable expiry", () => {
    for (const value of ["", "not a date"]) {
      expect(() =>
        buildSetupMintingDelegation({ ...fields, proposalDelegationExpiresAt: value }),
      ).toThrow("Expires At must be a valid date and time");
    }
  });

  it("refuses an expiry whose micros lose precision", () => {
    expect(() =>
      buildSetupMintingDelegation({ ...fields, proposalDelegationExpiresAt: "9999-01-01T00:00" }),
    ).toThrow("Expires At is too far in the future");
  });

  it("refuses a merge limit that is not a positive whole number", () => {
    for (const value of ["0", "-1", "2.5", "x"]) {
      expect(() =>
        buildSetupMintingDelegation({ ...fields, proposalAmuletMergeLimit: value }),
      ).toThrow("Amulet Merge Limit must be a positive integer");
    }
  });
});

describe("buildAcceptExternalPartySetup", () => {
  it("sends the trimmed proposal cid", () => {
    expect(buildAcceptExternalPartySetup({ proposalExternalPartySetupCid: " eps-1 " })).toEqual({
      type: "accept_external_party_setup",
      proposal_cid: "eps-1",
    });
  });

  it("refuses an empty cid", () => {
    expect(() => buildAcceptExternalPartySetup({ proposalExternalPartySetupCid: "  " })).toThrow(
      "External Party Setup Proposal Contract Id must not be empty",
    );
  });
});
