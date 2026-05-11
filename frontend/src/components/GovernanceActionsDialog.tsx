import {
  Dialog,
  DialogTitle,
  DialogContent,
  DialogActions,
  Button,
  Box,
} from "@mui/material";
import { GovernanceSection } from "./GovernanceSection";
import type { Network } from "../types";

interface GovernanceActionsDialogProps {
  open: boolean;
  onClose: () => void;
  partyId: string;
  rulesContractId: string;
  defaultOperatorParty?: string;
  network?: Network;
  governanceType: "vault" | "core_self" | "core_domain";
  onAfterAction?: () => void;
  /**
   * Which half of GovernanceSection to render:
   * - "actions"   = governance-action confirmations + new-action form (default, used by the pencil icon)
   * - "proposals" = domain-proposal list + new-proposal form (used by the header "New Proposal" button)
   */
  view?: "actions" | "proposals";
}

export const GovernanceActionsDialog = ({
  open,
  onClose,
  partyId,
  rulesContractId,
  defaultOperatorParty,
  network,
  governanceType,
  onAfterAction,
  view = "actions",
}: GovernanceActionsDialogProps) => {
  return (
    <Dialog open={open} onClose={onClose} maxWidth="sm" fullWidth>
      <DialogTitle>
        {view === "proposals" ? "New Proposal" : "Governance Actions"}
      </DialogTitle>
      <DialogContent>
        <Box sx={{ pt: 1 }}>
          <GovernanceSection
            partyId={partyId}
            rulesContractId={rulesContractId}
            governanceContractIds={[rulesContractId]}
            defaultOperatorParty={defaultOperatorParty}
            network={network}
            governanceType={governanceType}
            onAfterAction={onAfterAction}
            view={view}
          />
        </Box>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Close</Button>
      </DialogActions>
    </Dialog>
  );
};
