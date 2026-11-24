import type { ProposalState } from "./useProposalState";
import type { AppRewardBeneficiary, ProposalType } from "../../../types";

// The proposal payload of the Set Provider App Reward Beneficiaries form.

const buildSetProviderAppRewardBeneficiaries = ({
  proposalInstrumentConfigurationCid,
  proposalClearBeneficiaries,
  proposalBeneficiaries,
  beneficiaryProblem,
}: Pick<
  ProposalState,
  | "proposalInstrumentConfigurationCid"
  | "proposalClearBeneficiaries"
  | "proposalBeneficiaries"
  | "beneficiaryProblem"
>): ProposalType => {
  if (beneficiaryProblem) throw new Error(beneficiaryProblem);
  let beneficiaries: AppRewardBeneficiary[] | null = null;
  if (!proposalClearBeneficiaries) {
    beneficiaries = proposalBeneficiaries.map((b) => ({
      beneficiary: b.beneficiary.trim(),
      weight: b.weight.trim(),
    }));
  }
  return {
    type: "set_provider_app_reward_beneficiaries",
    instrument_configuration_cid: proposalInstrumentConfigurationCid,
    provider_app_reward_beneficiaries: beneficiaries ?? undefined,
  };
};

export {
  buildSetProviderAppRewardBeneficiaries,
};
