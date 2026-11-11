import { buildBillingParams, buildInitialDeposit } from "../../../paidCredential";
import { parseClaimsText } from "../proposalParse";
import type { ProposalState } from "./useProposalState";
import type { ProposalType } from "../../../types";

// The proposal payloads of the credential forms.

const buildOfferFreeCredential = ({
  proposalDescription,
  proposalHolder,
  proposalUserServiceCid,
  proposalCredentialId,
  proposalCredentialClaimsText,
}: Pick<
  ProposalState,
  | "proposalDescription"
  | "proposalHolder"
  | "proposalUserServiceCid"
  | "proposalCredentialId"
  | "proposalCredentialClaimsText"
>): ProposalType => {
  const claims = parseClaimsText(proposalCredentialClaimsText);
  return {
    type: "offer_free_credential",
    user_service_cid: proposalUserServiceCid,
    holder: proposalHolder,
    id: proposalCredentialId,
    description: proposalDescription,
    claims,
  };
};

const buildAcceptFreeCredential = ({
  proposalUserServiceCid,
  proposalCredentialOfferCid,
}: Pick<
  ProposalState,
  | "proposalUserServiceCid"
  | "proposalCredentialOfferCid"
>): ProposalType => {
  return {
    type: "accept_free_credential",
    user_service_cid: proposalUserServiceCid,
    credential_offer_cid: proposalCredentialOfferCid,
  };
};

const buildOfferPaidCredential = ({
  proposalDescription,
  proposalHolder,
  proposalUserServiceCid,
  proposalCredentialId,
  proposalCredentialClaimsText,
  proposalFeePerDayUsd,
  proposalBillingPeriodMinutes,
  proposalDepositTargetAmountUsd,
  proposalHolderActivityWeight,
  proposalDepositInitialAmountUsd,
}: Pick<
  ProposalState,
  | "proposalDescription"
  | "proposalHolder"
  | "proposalUserServiceCid"
  | "proposalCredentialId"
  | "proposalCredentialClaimsText"
  | "proposalFeePerDayUsd"
  | "proposalBillingPeriodMinutes"
  | "proposalDepositTargetAmountUsd"
  | "proposalHolderActivityWeight"
  | "proposalDepositInitialAmountUsd"
>): ProposalType => {
  return {
    type: "offer_paid_credential",
    user_service_cid: proposalUserServiceCid,
    holder: proposalHolder,
    id: proposalCredentialId,
    description: proposalDescription,
    claims: parseClaimsText(proposalCredentialClaimsText),
    billing_params: buildBillingParams({
      feePerDayUsd: proposalFeePerDayUsd,
      billingPeriodMinutes: proposalBillingPeriodMinutes,
      depositTargetAmountUsd: proposalDepositTargetAmountUsd,
      holderActivityWeight: proposalHolderActivityWeight,
    }),
    ...buildInitialDeposit(proposalDepositInitialAmountUsd),
  };
};

const buildAcceptPaidCredential = ({
  proposalUserServiceCid,
  proposalCredentialOfferCid,
  proposalDepositAmuletCids,
  proposalOpenMiningRoundCid,
  proposalFeaturedAppRightCid,
  amuletRulesCid,
  chosenDepositUsd,
}: Pick<
  ProposalState,
  | "proposalUserServiceCid"
  | "proposalCredentialOfferCid"
  | "proposalDepositAmuletCids"
  | "proposalOpenMiningRoundCid"
  | "proposalFeaturedAppRightCid"
  | "amuletRulesCid"
  | "chosenDepositUsd"
>): ProposalType => {
  if (proposalDepositAmuletCids.length === 0) {
    throw new Error("Pick at least one Deposit Amulet");
  }
  const featuredAppRightCid = proposalFeaturedAppRightCid.trim();
  return {
    type: "accept_paid_credential",
    user_service_cid: proposalUserServiceCid,
    credential_offer_cid: proposalCredentialOfferCid,
    deposit_amulet_cids: proposalDepositAmuletCids,
    app_transfer_context: {
      amulet_rules_cid: amuletRulesCid.trim(),
      open_mining_round_cid: proposalOpenMiningRoundCid.trim(),
      ...(featuredAppRightCid ? { featured_app_right_cid: featuredAppRightCid } : {}),
    },
    // The vote's description shows this amount, and execution checks
    // it against the offer.
    ...(chosenDepositUsd ? { deposit_amount_usd: chosenDepositUsd } : {}),
  };
};

export {
  buildOfferFreeCredential,
  buildAcceptFreeCredential,
  buildOfferPaidCredential,
  buildAcceptPaidCredential,
};
