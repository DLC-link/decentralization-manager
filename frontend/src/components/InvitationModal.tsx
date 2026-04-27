import { useState } from "react";
import {
  Dialog,
  DialogTitle,
  DialogContent,
  DialogActions,
  Button,
  Typography,
  Box,
  CircularProgress,
  Chip,
} from "@mui/material";
import { API_BASE } from "../constants";
import { authenticatedFetch } from "../api";
import { useSnackbar } from "../contexts";
import type { PendingInvitation } from "../types";

interface InvitationModalProps {
  invitation: PendingInvitation | null;
  onClose: () => void;
  onAction: () => void;
}

const getInvitationTitle = (type: string): string => {
  switch (type) {
    case "Onboarding":
      return "Join Decentralized Party";
    case "Kick":
      return "Participant Removal";
    case "Contracts":
      return "Contract Deployment";
    case "Dars":
      return "DAR Upload";
    default:
      return "Workflow Invitation";
  }
};

const getInvitationDescription = (type: string): string => {
  switch (type) {
    case "Onboarding":
      return "You have been invited to join a new decentralized party. Accepting will start the onboarding workflow on your node.";
    case "Kick":
      return "A participant removal workflow has been initiated. Accepting will start the kick workflow on your node.";
    case "Contracts":
      return "A contract deployment workflow has been initiated. Accepting will start the contracts workflow on your node.";
    case "Dars":
      return "A DAR upload workflow has been initiated. Accepting will install the DAR packages on your node.";
    default:
      return "You have been invited to participate in a workflow.";
  }
};

const getChipColor = (type: string): "primary" | "warning" | "info" => {
  switch (type) {
    case "Onboarding":
      return "primary";
    case "Kick":
      return "warning";
    case "Contracts":
    case "Dars":
      return "info";
    default:
      return "primary";
  }
};

export const InvitationModal = ({
  invitation,
  onClose,
  onAction,
}: InvitationModalProps) => {
  const [loading, setLoading] = useState(false);
  const { showSnackbar } = useSnackbar();

  const handleAccept = async () => {
    if (!invitation) return;
    setLoading(true);

    try {
      const res = await authenticatedFetch(`${API_BASE}/invitations/accept`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ id: invitation.id }),
      });

      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || "Failed to accept invitation");
      }

      showSnackbar("Invitation accepted - workflow started");
      onAction();
    } catch (err) {
      showSnackbar(err instanceof Error ? err.message : "Failed to accept invitation");
    } finally {
      setLoading(false);
    }
  };

  const handleDecline = async () => {
    if (!invitation) return;
    setLoading(true);

    try {
      const res = await authenticatedFetch(`${API_BASE}/invitations/decline`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ id: invitation.id }),
      });

      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || "Failed to decline invitation");
      }

      showSnackbar("Invitation declined");
      onAction();
    } catch (err) {
      showSnackbar(err instanceof Error ? err.message : "Failed to decline invitation");
    } finally {
      setLoading(false);
    }
  };

  if (!invitation) return null;

  const receivedDate = new Date(invitation.received_at * 1000);

  return (
    <Dialog open={!!invitation} onClose={onClose} maxWidth="sm" fullWidth>
      <DialogTitle sx={{ display: "flex", alignItems: "center", gap: 2 }}>
        <Chip
          label={invitation.invitation_type}
          color={getChipColor(invitation.invitation_type)}
          size="small"
        />
        {getInvitationTitle(invitation.invitation_type)}
      </DialogTitle>
      <DialogContent>
        <Box sx={{ display: "flex", flexDirection: "column", gap: 2, mt: 1 }}>
          <Typography variant="body1">
            {getInvitationDescription(invitation.invitation_type)}
          </Typography>

          <Box sx={{ bgcolor: "action.hover", p: 2, borderRadius: 1 }}>
            <Typography variant="body2" color="text.secondary" gutterBottom>
              From:
            </Typography>
            <Typography variant="body2" sx={{ fontFamily: "monospace" }}>
              {invitation.coordinator_name || invitation.coordinator_pubkey.slice(0, 32) + "..."}
            </Typography>

            <Typography variant="body2" color="text.secondary" sx={{ mt: 1 }} gutterBottom>
              Received:
            </Typography>
            <Typography variant="body2">
              {receivedDate.toLocaleString()}
            </Typography>
          </Box>
        </Box>
      </DialogContent>
      <DialogActions>
        <Button onClick={handleDecline} disabled={loading} color="inherit">
          Decline
        </Button>
        <Button
          onClick={handleAccept}
          variant="contained"
          disabled={loading}
        >
          {loading ? <CircularProgress size={20} /> : "Accept"}
        </Button>
      </DialogActions>
    </Dialog>
  );
};
