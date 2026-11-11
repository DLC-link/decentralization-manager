import { describe, expect, it } from "vitest";

import { buildSetProviderAppRewardBeneficiaries } from "./rewardBeneficiariesPayload";

const fields = {
  proposalInstrumentConfigurationCid: "inst-1",
  proposalClearBeneficiaries: false,
  proposalBeneficiaries: [
    { beneficiary: " alice::1 ", weight: " 0.25 " },
    { beneficiary: "bob::1", weight: "0.75" },
  ],
  beneficiaryProblem: null,
};

describe("buildSetProviderAppRewardBeneficiaries", () => {
  it("sends the trimmed rows", () => {
    expect(buildSetProviderAppRewardBeneficiaries(fields)).toEqual({
      type: "set_provider_app_reward_beneficiaries",
      instrument_configuration_cid: "inst-1",
      provider_app_reward_beneficiaries: [
        { beneficiary: "alice::1", weight: "0.25" },
        { beneficiary: "bob::1", weight: "0.75" },
      ],
    });
  });

  it("leaves the list out to clear it, whatever the rows hold", () => {
    expect(
      buildSetProviderAppRewardBeneficiaries({ ...fields, proposalClearBeneficiaries: true }),
    ).toEqual({
      type: "set_provider_app_reward_beneficiaries",
      instrument_configuration_cid: "inst-1",
      provider_app_reward_beneficiaries: undefined,
    });
  });

  it("refuses the list when the form found a problem", () => {
    expect(() =>
      buildSetProviderAppRewardBeneficiaries({
        ...fields,
        beneficiaryProblem: "Weights must sum to 1",
      }),
    ).toThrow("Weights must sum to 1");
  });
});
