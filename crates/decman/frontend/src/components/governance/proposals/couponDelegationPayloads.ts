import {
  formatSplitShare,
  parseSplitWeights,
  splitFromWeights,
  validateDelegationSplit,
} from "../rewardSplit";
import type { ProposalState } from "./useProposalState";
import type { ProposalType } from "../../../types";

// The proposal payloads of the coupon reassignment delegation forms.

const buildSetupCouponReassignmentDelegation = ({
  proposalDelegationDso,
  proposalDelegationAssigners,
  proposalDelegationSplit,
  proposalPriorDelegation,
}: Pick<
  ProposalState,
  | "proposalDelegationDso"
  | "proposalDelegationAssigners"
  | "proposalDelegationSplit"
  | "proposalPriorDelegation"
>): ProposalType => {
  const assigners = proposalDelegationAssigners
    .map((a) => a.trim())
    .filter((a) => a.length > 0);
  if (assigners.length === 0) {
    throw new Error("At least one assigner is required");
  }
  if (new Set(assigners).size !== assigners.length) {
    throw new Error("Assigners must be unique");
  }
  const splitError = validateDelegationSplit(proposalDelegationSplit);
  if (splitError) {
    throw new Error(splitError);
  }
  // Submit the derived decimals, not the weights — the delegation bakes
  // in exact percentages and that is what a confirmer reviews.
  const weights = parseSplitWeights(proposalDelegationSplit)!;
  const shares = splitFromWeights(weights)!;
  const split = proposalDelegationSplit.map((b, idx) => ({
    beneficiary: b.beneficiary.trim(),
    percentage: formatSplitShare(shares[idx]),
  }));
  return {
    type: "setup_coupon_reassignment_delegation",
    dso: proposalDelegationDso.trim(),
    assigners,
    new_beneficiaries: split,
    prior_delegation: proposalPriorDelegation.trim() || undefined,
  };
};

const buildRevokeCouponReassignmentDelegation = ({
  proposalRevokeDelegationCid,
}: Pick<ProposalState, "proposalRevokeDelegationCid">): ProposalType => {
  return {
    type: "revoke_coupon_reassignment_delegation",
    delegation: proposalRevokeDelegationCid.trim(),
  };
};

export {
  buildSetupCouponReassignmentDelegation,
  buildRevokeCouponReassignmentDelegation,
};
