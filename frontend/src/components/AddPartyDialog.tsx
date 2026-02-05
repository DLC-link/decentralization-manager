import { useState, useEffect, useCallback } from "react";
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
  FormControl,
  InputLabel,
  Select,
  MenuItem,
} from "@mui/material";
import { API_BASE } from "../constants";
import type { AddPartyRequest, AddPartyStatusResponse, Peer } from "../types";

interface AddPartyDialogProps {
  open: boolean;
  onClose: () => void;
  onAddComplete: () => void;
  partyId: string;
  currentThreshold: number;
  currentOwnerCount: number;
  peers: Peer[];
  currentParticipantIds: string[];
}

export const AddPartyDialog = ({
  open,
  onClose,
  onAddComplete,
  partyId,
  currentThreshold,
  currentOwnerCount,
  peers,
  currentParticipantIds,
}: AddPartyDialogProps) => {
  const [newParticipantId, setNewParticipantId] = useState("");
  const [newThreshold, setNewThreshold] = useState(1);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<AddPartyStatusResponse | null>(null);

  // Calculate suggested threshold when owner count changes
  const newOwnerCount = currentOwnerCount + 1;
  const suggestedThreshold = Math.max(1, Math.ceil(newOwnerCount / 2));

  // Filter peers to only show those not already participants
  const availablePeers = peers.filter(
    (peer) => !currentParticipantIds.includes(peer.participant_id)
  );

  useEffect(() => {
    // Reset threshold to suggested value when dialog opens with new data
    setNewThreshold(Math.max(currentThreshold, suggestedThreshold));
  }, [currentThreshold, suggestedThreshold]);

  useEffect(() => {
    if (!open) {
      setError(null);
      setStatus(null);
      setLoading(false);
      setNewParticipantId("");
    }
  }, [open]);

  const pollStatus = useCallback(async () => {
    try {
      const res = await fetch(`${API_BASE}/add-party/status`);
      if (res.ok) {
        const data: AddPartyStatusResponse = await res.json();
        setStatus(data);
        if (data.status !== "inprogress") {
          setLoading(false);
          if (data.status === "completed") {
            onAddComplete();
          }
        }
      }
    } catch {
      // Ignore polling errors
    }
  }, [onAddComplete]);

  useEffect(() => {
    let interval: number | undefined;

    if (status?.status === "inprogress") {
      // Poll immediately, then every 2 seconds
      pollStatus();
      interval = window.setInterval(pollStatus, 2000);
    }

    return () => {
      if (interval) clearInterval(interval);
    };
  }, [status?.status, pollStatus]);

  const handleAddParty = async () => {
    setLoading(true);
    setError(null);

    const request: AddPartyRequest = {
      decentralized_party_id: partyId,
      new_participant_id: newParticipantId,
      new_threshold: newThreshold,
    };

    try {
      const res = await fetch(`${API_BASE}/add-party`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });

      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || "Failed to start add party workflow");
      }

      setStatus({ status: "inprogress" });
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
      <DialogTitle>Add Participant</DialogTitle>
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
          />

          <FormControl fullWidth size="small" disabled={loading}>
            <InputLabel>New Participant</InputLabel>
            <Select
              value={newParticipantId}
              label="New Participant"
              onChange={(e) => setNewParticipantId(e.target.value)}
            >
              {availablePeers.map((peer) => (
                <MenuItem key={peer.participant_id} value={peer.participant_id}>
                  {peer.name} ({peer.participant_id.slice(0, 32)}...)
                </MenuItem>
              ))}
            </Select>
          </FormControl>

          {availablePeers.length === 0 && (
            <Alert severity="info">
              No available peers to add. All configured peers are already
              participants.
            </Alert>
          )}

          <TextField
            label="New Threshold"
            type="number"
            value={newThreshold}
            onChange={(e) =>
              setNewThreshold(Math.max(1, parseInt(e.target.value) || 1))
            }
            fullWidth
            size="small"
            disabled={loading}
            slotProps={{ htmlInput: { min: 1, max: newOwnerCount } }}
            helperText={`Threshold after adding (suggested: ${suggestedThreshold}, max: ${newOwnerCount})`}
          />

          {error && <Alert severity="error">{error}</Alert>}

          {status?.status === "inprogress" && (
            <Alert severity="info" icon={<CircularProgress size={20} />}>
              Add party workflow in progress... This may take a few minutes.
            </Alert>
          )}

          {status?.status === "completed" && (
            <Alert severity="success">
              Participant has been successfully added to the party.
            </Alert>
          )}

          {status?.status === "failed" && (
            <Alert severity="error">
              Add party workflow failed: {status.error || "Unknown error"}
            </Alert>
          )}
        </Box>
      </DialogContent>
      <DialogActions>
        <Button onClick={handleClose} disabled={loading}>
          {status?.status === "completed" || status?.status === "failed"
            ? "Close"
            : "Cancel"}
        </Button>
        {!status?.status ||
        status.status === "idle" ||
        status.status === "failed" ? (
          <Button
            onClick={handleAddParty}
            variant="contained"
            color="primary"
            disabled={
              loading ||
              !newParticipantId ||
              newThreshold < 1 ||
              newThreshold > newOwnerCount
            }
          >
            {loading ? <CircularProgress size={20} /> : "Add Participant"}
          </Button>
        ) : null}
      </DialogActions>
    </Dialog>
  );
};
