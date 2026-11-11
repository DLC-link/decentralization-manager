import type { ProposalState } from "./useProposalState";
import type { ProposalType } from "../../../types";

// The proposal payloads of the Generic Vote and token custody forms.

const buildSetupCcPreapproval = ({
  proposalProvider,
  proposalExpectedDso,
}: Pick<ProposalState, "proposalProvider" | "proposalExpectedDso">): ProposalType => {
  return {
    type: "setup_cc_preapproval",
    provider: proposalProvider,
    expected_dso: proposalExpectedDso,
  };
};

const buildSetupTokenPreapproval = ({
  proposalOperator,
  proposalInstrumentAdmin,
  proposalInstrumentAllowances,
}: Pick<ProposalState, "proposalOperator" | "proposalInstrumentAdmin" | "proposalInstrumentAllowances">): ProposalType => {
  return {
    type: "setup_token_preapproval",
    operator: proposalOperator,
    instrument_admin: proposalInstrumentAdmin,
    // Strip the local-only `uid` field and drop empty rows.
    instrument_allowances: proposalInstrumentAllowances
      .filter((a) => a.id.trim() !== "")
      .map(({ id }) => ({ id })),
  };
};

const buildGenericVote = ({
  proposalDescription,
}: Pick<ProposalState, "proposalDescription">): ProposalType => {
  return {
    type: "generic_vote",
    description: proposalDescription,
  };
};

export {
  buildSetupCcPreapproval,
  buildSetupTokenPreapproval,
  buildGenericVote,
};
