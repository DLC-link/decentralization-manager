// Input handling for the Offer Paid Credential and Accept Paid Credential
// proposal forms.

import type { BillingParams, ContractWithBlob } from "./types";

/// The billing fields of the Offer Paid Credential form, as typed.
export interface BillingInputs {
  feePerDayUsd: string;
  billingPeriodMinutes: string;
  depositTargetAmountUsd: string;
  holderActivityWeight: string;
}

const DECIMAL = /^\d+(\.\d+)?$/;

const requirePositive = (value: string, label: string): string => {
  const v = value.trim();
  if (!DECIMAL.test(v) || Number(v) <= 0) {
    throw new Error(`${label} must be a number above 0`);
  }
  return v;
};

/// The billing params to post, or an error naming the field to fix. The
/// checks are those of `validateBillingParams` in utility-credential-app-v0.
/// The offer applies them only at execution, after the vote.
export const buildBillingParams = (inputs: BillingInputs): BillingParams => {
  const fee = requirePositive(inputs.feePerDayUsd, "Fee per Day (USD)");
  const period = inputs.billingPeriodMinutes.trim();
  if (!/^\d+$/.test(period) || Number(period) <= 0 || !Number.isSafeInteger(Number(period))) {
    throw new Error("Billing Period (minutes) must be a whole number above 0");
  }
  const target = requirePositive(inputs.depositTargetAmountUsd, "Deposit Target (USD)");
  const weight = inputs.holderActivityWeight.trim();
  if (weight && (!DECIMAL.test(weight) || Number(weight) > 1)) {
    throw new Error("Holder Activity Weight must be a number from 0 to 1");
  }
  return {
    fee_per_day_usd: fee,
    billing_period_minutes: Number(period),
    deposit_target_amount_usd: target,
    ...(weight ? { holder_activity_weight: weight } : {}),
  };
};

/// The optional initial deposit, or an error when it is set but not above 0.
export const buildInitialDeposit = (value: string): { deposit_initial_amount_usd?: string } =>
  value.trim()
    ? { deposit_initial_amount_usd: requirePositive(value, "Initial Deposit (USD)") }
    : {};

/// One Amulet the governance party can lock as a deposit.
export interface AmuletOption {
  contract_id: string;
  /// `amount.initialAmount`, the figure the credential app checks against
  /// the deposit.
  amount: string;
}

/// The Amulets `owner` holds, from a `/contracts/query` response that
/// includes payloads. Amulets of any other owner are dropped.
export const ownedAmulets = (contracts: ContractWithBlob[], owner: string): AmuletOption[] =>
  contracts.flatMap((c) => {
    const payload = c.payload as
      | { owner?: unknown; amount?: { initialAmount?: unknown } }
      | undefined;
    const amount = payload?.amount?.initialAmount;
    if (payload?.owner !== owner || typeof amount !== "string") return [];
    return [{ contract_id: c.contract_id, amount }];
  });
