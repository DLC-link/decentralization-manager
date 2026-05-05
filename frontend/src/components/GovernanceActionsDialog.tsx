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
  memberPartyId: string;
  defaultOperatorParty?: string;
  network?: Network;
  governanceType: "vault" | "core_self" | "core_domain";
  onAfterAction?: () => void;
}

export const GovernanceActionsDialog = ({
  open,
  onClose,
  partyId,
  rulesContractId,
  memberPartyId,
  defaultOperatorParty,
  network,
  governanceType,
  onAfterAction,
}: GovernanceActionsDialogProps) => {
  return (
    <Dialog open={open} onClose={onClose} maxWidth="lg" fullWidth>
      <DialogTitle>Governance Actions</DialogTitle>
      <DialogContent>
        <Box sx={{ pt: 1 }}>
          <GovernanceSection
            partyId={partyId}
            rulesContractId={rulesContractId}
            governanceContractIds={[rulesContractId]}
            memberPartyId={memberPartyId}
            defaultOperatorParty={defaultOperatorParty}
            network={network}
            governanceType={governanceType}
            onAfterAction={onAfterAction}
          />
        </Box>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Close</Button>
      </DialogActions>
    </Dialog>
  );
};
