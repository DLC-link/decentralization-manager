import { describe, expect, it } from "vitest";

import { formatDecimal, offerBillingSummary, offerDepositUsd } from "./paidCredential";
import type { CredentialOfferInfo } from "./types";

const ns = "a".repeat(68);

const paidOffer = (deposit_initial_amount_usd?: string): CredentialOfferInfo => ({
  contract_id: "00offer",
  operator: `operator::${ns}`,
  issuer: `issuer::${ns}`,
  holder: `gov::${ns}`,
  credential_id: "paid-1",
  description: "",
  is_free: false,
  billing_params: {
    fee_per_day_usd: "1.5000000000",
    billing_period_minutes: 60,
    deposit_target_amount_usd: "30.0000000000",
  },
  ...(deposit_initial_amount_usd ? { deposit_initial_amount_usd } : {}),
});

describe("formatDecimal", () => {
  it.each([
    ["30.0000000000", "30"],
    ["1.5000000000", "1.5"],
    ["100", "100"],
    ["0.0100000000", "0.01"],
  ])("shows %s as %s", (value, shown) => {
    expect(formatDecimal(value)).toBe(shown);
  });
});

describe("offerDepositUsd", () => {
  it("is the deposit target when the offer sets no initial deposit", () => {
    expect(offerDepositUsd(paidOffer())).toBe("30.0000000000");
  });

  it("is the initial deposit when the offer sets one", () => {
    expect(offerDepositUsd(paidOffer("20.0000000000"))).toBe("20.0000000000");
  });

  it("is undefined for an offer without billing params", () => {
    expect(offerDepositUsd({ ...paidOffer(), billing_params: undefined })).toBeUndefined();
  });
});

describe("offerBillingSummary", () => {
  it("names the deposit and the daily fee", () => {
    expect(offerBillingSummary(paidOffer("20.0000000000"))).toBe(
      "Deposit 20 USD, fee 1.5 USD a day",
    );
  });
});
