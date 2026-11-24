import { describe, expect, it } from "vitest";

import {
  buildAcceptFreeCredential,
  buildAcceptPaidCredential,
  buildOfferFreeCredential,
  buildOfferPaidCredential,
} from "./credentialPayloads";

const offer = {
  proposalDescription: "KYC passed",
  proposalHolder: "holder::1",
  proposalUserServiceCid: "us-1",
  proposalCredentialId: "kyc/1",
  proposalCredentialClaimsText: "holder::1,kyc,true",
};

const billing = {
  proposalFeePerDayUsd: "1.5",
  proposalBillingPeriodMinutes: "60",
  proposalDepositTargetAmountUsd: "100",
  proposalHolderActivityWeight: "",
  proposalDepositInitialAmountUsd: "",
};

describe("buildOfferFreeCredential", () => {
  it("parses the claims into the payload", () => {
    expect(buildOfferFreeCredential(offer)).toEqual({
      type: "offer_free_credential",
      user_service_cid: "us-1",
      holder: "holder::1",
      id: "kyc/1",
      description: "KYC passed",
      claims: [{ subject: "holder::1", property: "kyc", value: "true" }],
    });
  });

  it("refuses a bad claim line", () => {
    expect(() =>
      buildOfferFreeCredential({ ...offer, proposalCredentialClaimsText: "kyc,true" }),
    ).toThrow("Claim line 1");
  });
});

describe("buildOfferPaidCredential", () => {
  it("adds the billing params and leaves the initial deposit out when empty", () => {
    expect(buildOfferPaidCredential({ ...offer, ...billing })).toEqual({
      type: "offer_paid_credential",
      user_service_cid: "us-1",
      holder: "holder::1",
      id: "kyc/1",
      description: "KYC passed",
      claims: [{ subject: "holder::1", property: "kyc", value: "true" }],
      billing_params: {
        fee_per_day_usd: "1.5",
        billing_period_minutes: 60,
        deposit_target_amount_usd: "100",
      },
    });
  });

  it("sends the initial deposit when it is set", () => {
    expect(
      buildOfferPaidCredential({ ...offer, ...billing, proposalDepositInitialAmountUsd: " 25 " }),
    ).toMatchObject({ deposit_initial_amount_usd: "25" });
  });

  it("checks the claims before the billing fields", () => {
    expect(() =>
      buildOfferPaidCredential({
        ...offer,
        ...billing,
        proposalCredentialClaimsText: "bad",
        proposalFeePerDayUsd: "0",
      }),
    ).toThrow("Claim line 1");
  });
});

describe("buildAcceptFreeCredential", () => {
  it("names the user service and the offer", () => {
    expect(
      buildAcceptFreeCredential({
        proposalUserServiceCid: "us-1",
        proposalCredentialOfferCid: "offer-1",
      }),
    ).toEqual({
      type: "accept_free_credential",
      user_service_cid: "us-1",
      credential_offer_cid: "offer-1",
    });
  });
});

describe("buildAcceptPaidCredential", () => {
  const accept = {
    proposalUserServiceCid: "us-1",
    proposalCredentialOfferCid: "offer-1",
    proposalDepositAmuletCids: ["amulet-1", "amulet-2"],
    proposalOpenMiningRoundCid: " round-1 ",
    proposalFeaturedAppRightCid: " ",
    amuletRulesCid: " rules-1 ",
    chosenDepositUsd: undefined,
  };

  it("pins the trimmed transfer context and leaves the empty extras out", () => {
    expect(buildAcceptPaidCredential(accept)).toEqual({
      type: "accept_paid_credential",
      user_service_cid: "us-1",
      credential_offer_cid: "offer-1",
      deposit_amulet_cids: ["amulet-1", "amulet-2"],
      app_transfer_context: {
        amulet_rules_cid: "rules-1",
        open_mining_round_cid: "round-1",
      },
    });
  });

  it("adds the featured app right and the listed offer's deposit", () => {
    expect(
      buildAcceptPaidCredential({
        ...accept,
        proposalFeaturedAppRightCid: " far-1 ",
        chosenDepositUsd: "100",
      }),
    ).toMatchObject({
      app_transfer_context: { featured_app_right_cid: "far-1" },
      deposit_amount_usd: "100",
    });
  });

  it("needs at least one deposit amulet", () => {
    expect(() =>
      buildAcceptPaidCredential({ ...accept, proposalDepositAmuletCids: [] }),
    ).toThrow("Pick at least one Deposit Amulet");
  });
});
