import { Box, CircularProgress, IconButton, Tooltip } from "@mui/material";
import RefreshIcon from "@mui/icons-material/Refresh";
import { HelpTextField } from "../fields/HelpTextField";
import type { ProposalState } from "./useProposalState";

interface Props {
  state: ProposalState;
}

// Shared by the Request DevNet Featured App Right and Accept Paid Credential
// proposal forms: both pin the DSO's current AmuletRules.
export const AmuletRulesCidField = ({ state }: Props) => {
  const { amuletRulesCid, setAmuletRulesCid, amuletRulesLoading, fetchNetworkInfo } = state;

  return (
    <Box sx={{ display: "flex", gap: 1, alignItems: "center" }}>
      <HelpTextField
        label="Amulet Rules CID"
        value={amuletRulesCid}
        onChange={(e) => setAmuletRulesCid(e.target.value)}
        fullWidth
        size="small"
        required
        help="Contract id of the DSO's current AmuletRules contract, prefilled from the DSO scan API. Refresh it if the DSO has replaced the contract since this form opened."
      />
      <Tooltip title="Refresh">
        <span>
          <IconButton
            size="small"
            onClick={fetchNetworkInfo}
            disabled={amuletRulesLoading}
          >
            {amuletRulesLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
          </IconButton>
        </span>
      </Tooltip>
    </Box>
  );
};
