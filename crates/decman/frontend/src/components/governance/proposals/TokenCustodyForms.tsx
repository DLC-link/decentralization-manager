import { Alert, Box, Button, Typography } from "@mui/material";
import { TextHelp } from "../../FieldHelp";
import { HelpTextField } from "../fields/HelpTextField";
import type { ProposalState } from "./useProposalState";

interface Props {
  state: ProposalState;
}

/** The Generic Vote, Setup CC Preapproval and Setup Token Preapproval forms. */
export const TokenCustodyForms = ({ state }: Props) => {
  const {
    proposalType,
    proposalProvider,
    setProposalProvider,
    proposalExpectedDso,
    setProposalExpectedDso,
    proposalOperator,
    setProposalOperator,
    proposalInstrumentAdmin,
    setProposalInstrumentAdmin,
    proposalInstrumentAllowances,
    setProposalInstrumentAllowances,
    proposalDescription,
    setProposalDescription,
    preapprovalCounts,
  } = state;

  return (
    <>
      {proposalType === "generic_vote" && (
        <HelpTextField
          size="small"
          label="Vote Description"
          value={proposalDescription}
          onChange={(e) => setProposalDescription(e.target.value)}
          fullWidth
          required
          multiline
          minRows={2}
          maxRows={6}
          helperText="Describe what the governance members are voting on"
          help="Free-form human-readable note describing what governance members are voting on."
        />
      )}

      {proposalType === "setup_cc_preapproval" && (
        <>
          {preapprovalCounts.cc > 0 && (
            <Alert severity="warning">
              This party already has a Canton Coin TransferPreapproval;
              issuing another would create a duplicate and burn fees again.
            </Alert>
          )}
          <HelpTextField
            size="small"
            label="Provider Party"
            value={proposalProvider}
            onChange={(e) => setProposalProvider(e.target.value)}
            fullWidth
            required
            help="Party id of the app provider that will receive the TransferPreapproval (usually the Splice app provider)."
          />
          <HelpTextField
            size="small"
            label="Expected DSO Party"
            value={proposalExpectedDso}
            onChange={(e) => setProposalExpectedDso(e.target.value)}
            fullWidth
            required
            help="Party id of the Splice DSO; the proposal verifies the AmuletRules contract belongs to this DSO."
          />
        </>
      )}

      {proposalType === "setup_token_preapproval" && (
        <>
          {preapprovalCounts.token > 0 && (
            <Alert severity="warning">
              This party already has {preapprovalCounts.token} token
              TransferPreapproval{preapprovalCounts.token === 1 ? "" : "s"};
              issuing another for the same instrument would likely be redundant.
            </Alert>
          )}
          <HelpTextField
            size="small"
            label="Operator Party"
            value={proposalOperator}
            onChange={(e) => setProposalOperator(e.target.value)}
            fullWidth
            required
            help="Party id of the utility operator that runs the token registrar."
          />
          <HelpTextField
            size="small"
            label="Instrument Admin"
            value={proposalInstrumentAdmin}
            onChange={(e) => setProposalInstrumentAdmin(e.target.value)}
            fullWidth
            required
            help="Issuer party of the token whose TransferPreapproval is being set up."
          />
          <Typography variant="caption" sx={{ display: "block" }} color="text.secondary">
            <TextHelp text="Optional per-instrument allowance ids that limit which tokens this preapproval covers. Leave empty to cover all.">
              Instrument Allowances (optional)
            </TextHelp>
          </Typography>
          {proposalInstrumentAllowances.map((a) => (
            <Box key={a.uid} sx={{ display: "flex", gap: 1, mb: 1 }}>
              <HelpTextField
                label="Allowance ID"
                value={a.id}
                onChange={(e) =>
                  setProposalInstrumentAllowances((prev) =>
                    prev.map((row) =>
                      row.uid === a.uid
                        ? { ...row, id: e.target.value }
                        : row,
                    ),
                  )
                }
                size="small"
                sx={{ flex: 1 }}
                help="Identifier of an allowed instrument under this preapproval."
              />
              <Button
                size="small"
                color="error"
                onClick={() =>
                  setProposalInstrumentAllowances((prev) =>
                    prev.filter((row) => row.uid !== a.uid),
                  )
                }
              >
                Remove
              </Button>
            </Box>
          ))}
          <Button
            size="small"
            onClick={() =>
              setProposalInstrumentAllowances((prev) => [
                ...prev,
                { uid: crypto.randomUUID(), id: "" },
              ])
            }
          >
            Add Allowance
          </Button>
        </>
      )}
    </>
  );
};
