import { Box, Button, Checkbox, FormControlLabel, Typography } from "@mui/material";
import { beneficiaryWeightSum } from "../../../rewardBeneficiaries";
import { TextHelp } from "../../FieldHelp";
import { ContractSelect } from "../fields/ContractSelect";
import { HelpTextField } from "../fields/HelpTextField";
import type { ProposalState } from "./useProposalState";

interface Props {
  state: ProposalState;
}

/** The Set Provider App Reward Beneficiaries form. */
export const RewardBeneficiariesForm = ({ state }: Props) => {
  const {
    proposalType,
    proposalInstrumentConfigurationCid,
    setProposalInstrumentConfigurationCid,
    proposalClearBeneficiaries,
    setProposalClearBeneficiaries,
    proposalBeneficiaries,
    setProposalBeneficiaries,
    availableInstruments,
    instrumentsLoading,
    beneficiaryProblem,
  } = state;

  return (
    <>
      {proposalType === "set_provider_app_reward_beneficiaries" && (
        <>
          <ContractSelect
            label="InstrumentConfiguration"
            help="Which utility-issued instrument these beneficiaries apply to."
            value={proposalInstrumentConfigurationCid}
            onChange={setProposalInstrumentConfigurationCid}
            items={availableInstruments}
            loading={instrumentsLoading}
            loadingText="Loading instruments…"
            emptyText="No instruments found — run SetupUtility first"
            rowLabel={(inst) => <>{inst.instrument_id} ({inst.contract_id.slice(0, 8)}…)</>}
          />
          <FormControlLabel
            control={<Checkbox size="small" checked={proposalClearBeneficiaries} onChange={(e) => setProposalClearBeneficiaries(e.target.checked)} />}
            label={
              <TextHelp text="Check this to remove all beneficiaries instead of setting a new list.">
                Clear beneficiaries (set to None)
              </TextHelp>
            }
          />
          {!proposalClearBeneficiaries && (
            <>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ display: "block" }}
              >
                <TextHelp text="Parties that share this instrument's app rewards. Each row is a party plus a weight; weights are decimals and must sum to 1.0.">
                  Beneficiaries (add party + weight)
                </TextHelp>
              </Typography>
              {proposalBeneficiaries.map((b, idx) => (
                <Box
                  key={idx}
                  sx={{ display: "flex", gap: 1, mb: 1 }}
                >
                  <HelpTextField
                    label="Beneficiary Party"
                    value={b.beneficiary}
                    onChange={(e) => {
                      const updated = [...proposalBeneficiaries];
                      updated[idx] = {
                        ...b,
                        beneficiary: e.target.value,
                      };
                      setProposalBeneficiaries(updated);
                    }}
                    size="small"
                    sx={{ flex: 2 }}
                    help="Party id that receives this share of the instrument's app rewards."
                  />
                  <HelpTextField
                    label="Weight"
                    value={b.weight}
                    onChange={(e) => {
                      const updated = [...proposalBeneficiaries];
                      updated[idx] = { ...b, weight: e.target.value };
                      setProposalBeneficiaries(updated);
                    }}
                    size="small"
                    sx={{ flex: 1 }}
                    help="Fraction of rewards this beneficiary gets, as a decimal. All row weights must sum to 1.0."
                  />
                  <Button
                    size="small"
                    color="error"
                    onClick={() =>
                      setProposalBeneficiaries(
                        proposalBeneficiaries.filter(
                          (_, i) => i !== idx,
                        ),
                      )
                    }
                  >
                    Remove
                  </Button>
                </Box>
              ))}
              <Box
                sx={{
                  display: "flex",
                  alignItems: "center",
                  gap: 2,
                }}
              >
                <Button
                  size="small"
                  onClick={() =>
                    setProposalBeneficiaries([
                      ...proposalBeneficiaries,
                      { beneficiary: "", weight: "1" },
                    ])
                  }
                >
                  Add Beneficiary
                </Button>
                {(() => {
                  // Just the running total: what is wrong with the
                  // rows is the alert's job.
                  const sum = beneficiaryWeightSum(proposalBeneficiaries);
                  if (proposalBeneficiaries.length === 0 || sum === null) return null;
                  return (
                    <Typography
                      variant="caption"
                      color={sum === "1" ? "success.main" : "text.secondary"}
                    >
                      Sum: {sum}
                    </Typography>
                  );
                })()}
              </Box>
              {beneficiaryProblem && (
                <Typography variant="caption" color="error.main" role="alert">
                  {beneficiaryProblem}
                </Typography>
              )}
            </>
          )}
        </>
      )}
    </>
  );
};
