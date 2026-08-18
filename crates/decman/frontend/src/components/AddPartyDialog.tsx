import { useState, useEffect } from "react";
import {
  Dialog,
  DialogTitle,
  DialogContent,
  DialogActions,
  Button,
  TextField,
  Typography,
  CircularProgress,
  Alert,
  Box,
  MenuItem,
} from "@mui/material";
import { API_BASE } from "../constants";
import { authenticatedFetch } from "../api";
import { useSnackbar } from "../contexts";
import { fieldHelpAdornment } from "./FieldHelp";
import type { AddPartyRequest, Peer } from "../types";

interface AddPartyDialogProps {
  open: boolean;
  onClose: () => void;
  onAddComplete: () => void;
  partyId: string;
  /** Canton IDs of the participants already hosting the party — peers in
   * this list are excluded from the candidate dropdown. */
  participantUids: string[];
  currentThreshold: number;
  currentOwnerCount: number;
}

export const AddPartyDialog = ({
  open,
  onClose,
  onAddComplete,
  partyId,
  participantUids,
  currentThreshold,
  currentOwnerCount,
}: AddPartyDialogProps) => {
  const [newParticipantId, setNewParticipantId] = useState("");
  const [newThreshold, setNewThreshold] = useState(1);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [peers, setPeers] = useState<Peer[]>([]);
  const [loadingPeers, setLoadingPeers] = useState(false);
  const { showSnackbar } = useSnackbar();

  // Fetch peers when dialog opens
  useEffect(() => {
    if (open) {
      const fetchPeers = async () => {
        setLoadingPeers(true);
        try {
          const res = await authenticatedFetch(`${API_BASE}/network-config`);
          if (res.ok) {
            const data = await res.json();
            setPeers(data.peers || []);
          }
        } catch {
          // Ignore fetch errors
        } finally {
          setLoadingPeers(false);
        }
      };
      fetchPeers();
    }
  }, [open]);

  // Only peers that don't already host the party can be added.
  const candidatePeers = peers.filter(
    (p) => !participantUids.includes(p.participant_id),
  );

  // Calculate suggested threshold for the grown owner set
  const newOwnerCount = currentOwnerCount + 1;
  const suggestedThreshold = Math.ceil(newOwnerCount / 2);

  useEffect(() => {
    // Reset threshold to suggested value when dialog opens with new data
    setNewThreshold(suggestedThreshold);
  }, [suggestedThreshold]);

  useEffect(() => {
    if (!open) {
      setError(null);
      setLoading(false);
      setNewParticipantId("");
    }
  }, [open]);


  const handleAdd = async () => {
    setLoading(true);
    setError(null);

    const request: AddPartyRequest = {
      decentralized_party_id: partyId,
      new_participant_id: newParticipantId,
      new_threshold: newThreshold,
      previous_threshold: currentThreshold,
    };

    try {
      const res = await authenticatedFetch(`${API_BASE}/add-party`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });

      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || "Failed to start add member workflow");
      }

      showSnackbar("Add member workflow started — follow progress in the feed");
      onClose();
      // Jump to the Pending Approvals feed so the user lands on the run they
      // just started (refresh + navigate). Without this the dialog closes
      // back to the party detail and the in-flight add is easy to miss.
      onAddComplete();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Unknown error");
      setLoading(false);
    }
  };

  const handleClose = () => {
    if (!loading) {
      onClose();
    }
  };

  return (
    <Dialog open={open} onClose={handleClose} maxWidth="sm" fullWidth>
      <DialogTitle>Add Member</DialogTitle>
      <DialogContent>
        <Box sx={{ display: "flex", flexDirection: "column", gap: 2, mt: 1 }}>
          <Typography variant="body2" color="text.secondary">
            Add a new participant to the decentralized party. This action
            requires coordination with other participants.
          </Typography>

          <TextField
            label="Decentralized Party ID"
            value={partyId}
            disabled
            fullWidth
            size="small"
            slotProps={{
              input: {
                endAdornment: fieldHelpAdornment(
                  "The decentralized party the participant is being added to. Pre-filled from the party you opened — not editable.",
                  "Help for Decentralized Party ID",
                ),
              },
            }}
          />

          <TextField
            label="Participant to Add"
            value={newParticipantId}
            onChange={(e) => setNewParticipantId(e.target.value)}
            select
            fullWidth
            size="small"
            disabled={loading || loadingPeers || candidatePeers.length === 0}
            helperText={
              loadingPeers
                ? "Loading peers…"
                : candidatePeers.length === 0
                  ? "No candidates — every configured peer already hosts this party"
                  : "Peers that are not yet participants of this party"
            }
          >
            {candidatePeers.map((peer) => (
              <MenuItem key={peer.participant_id} value={peer.participant_id}>
                <Box>
                  <Typography variant="body2">
                    {peer.name || peer.participant_id}
                  </Typography>
                  <Typography variant="caption" color="text.secondary">
                    {peer.address}:{peer.port}
                  </Typography>
                </Box>
              </MenuItem>
            ))}
          </TextField>

          <TextField
            label="New Threshold"
            type="number"
            value={newThreshold}
            onChange={(e) => setNewThreshold(Math.max(1, parseInt(e.target.value) || 1))}
            fullWidth
            size="small"
            disabled={loading}
            slotProps={{
              htmlInput: { min: 1, max: newOwnerCount },
              input: {
                endAdornment: fieldHelpAdornment(
                  "Number of owners that must sign topology changes for this party after the add. Must be between 1 and the number of owners including the new member.",
                  "Help for New Threshold",
                ),
              },
            }}
            helperText={`Threshold after add (suggested: ${suggestedThreshold}, max: ${newOwnerCount})`}
          />

          {error && (
            <Alert severity="error" onClose={() => setError(null)}>
              {error}
            </Alert>
          )}
        </Box>
      </DialogContent>
      <DialogActions>
        <Button onClick={handleClose} disabled={loading}>
          Cancel
        </Button>
        <Button
          onClick={handleAdd}
          variant="contained"
          color="primary"
          disabled={
            loading ||
            !newParticipantId ||
            newThreshold < 1 ||
            newThreshold > newOwnerCount
          }
        >
          {loading ? <CircularProgress size={20} /> : "Add Member"}
        </Button>
      </DialogActions>
    </Dialog>
  );
};
