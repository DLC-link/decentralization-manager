import type { ProposalState } from "./useProposalState";
import type { ProposalType } from "../../../types";

// The proposal payloads of the reward collection forms.

const buildSetupMintingDelegation = ({
  proposalExpectedDso,
  proposalDelegate,
  proposalDelegationExpiresAt,
  proposalAmuletMergeLimit,
  proposalDescription,
}: Pick<
  ProposalState,
  | "proposalExpectedDso"
  | "proposalDelegate"
  | "proposalDelegationExpiresAt"
  | "proposalAmuletMergeLimit"
  | "proposalDescription"
>): ProposalType => {
  const expiresAtMs = new Date(proposalDelegationExpiresAt).getTime();
  if (!Number.isFinite(expiresAtMs)) {
    throw new Error("Expires At must be a valid date and time");
  }
  // Micros beyond MAX_SAFE_INTEGER (~year 2255) would silently lose
  // precision; the datetime-local input allows dates up to year 9999.
  if (!Number.isSafeInteger(expiresAtMs * 1000)) {
    throw new Error("Expires At is too far in the future");
  }
  const mergeLimit = Number(proposalAmuletMergeLimit);
  if (!Number.isInteger(mergeLimit) || mergeLimit <= 0) {
    throw new Error("Amulet Merge Limit must be a positive integer");
  }
  return {
    type: "setup_minting_delegation",
    delegate: proposalDelegate,
    dso: proposalExpectedDso,
    expires_at_micros: expiresAtMs * 1000,
    amulet_merge_limit: mergeLimit,
    description: proposalDescription,
  };
};

const buildAcceptExternalPartySetup = ({
  proposalExternalPartySetupCid,
}: Pick<ProposalState, "proposalExternalPartySetupCid">): ProposalType => {
  const proposalCid = proposalExternalPartySetupCid.trim();
  if (!proposalCid) {
    throw new Error(
      "External Party Setup Proposal Contract Id must not be empty",
    );
  }
  return {
    type: "accept_external_party_setup",
    proposal_cid: proposalCid,
  };
};

export {
  buildSetupMintingDelegation,
  buildAcceptExternalPartySetup,
};
