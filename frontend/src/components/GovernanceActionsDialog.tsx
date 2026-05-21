import { useState } from "react";
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
  // Host node for `GovernanceSection`'s primary submit button. Pushing the
  // button into `DialogActions` keeps it inline with the Close button so
  // the dialog footer feels uniform — `GovernanceSection` portals it here
  // when `submitPortalEl` is non-null.
  const [submitHostEl, setSubmitHostEl] = useState<HTMLDivElement | null>(
    null,
  );

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
            onProposalCreated={onClose}
            view={view}
            submitPortalEl={submitHostEl}
          />
        </Box>
      </DialogContent>
      <DialogActions>
        {/* Submit goes first so it sits on the left of Close (primary
            action takes the visually-leading position). GovernanceSection
            portals its Submit Confirmation / Submit Proposal button into
            this slot. */}
        <Box ref={setSubmitHostEl} sx={{ display: "inline-flex" }} />
        <Button onClick={onClose}>Close</Button>
      </DialogActions>
    </Dialog>
  );
};
