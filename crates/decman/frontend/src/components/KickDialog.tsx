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
} from "@mui/material";
import { API_BASE } from "../constants";
import { authenticatedFetch } from "../api";
import { useSnackbar } from "../contexts";
import { fieldHelpAdornment } from "./FieldHelp";
import type {
  DecentralizedPartiesResponse,
  KickRequest,
  KickStatusResponse,
} from "../types";

interface KickDialogProps {
  open: boolean;
  onClose: () => void;
  onKickComplete: () => void;
  partyId: string;
  participantUid: string;
  participantOwnerKey?: string;
  currentThreshold: number;
  currentOwnerCount: number;
}

export const KickDialog = ({
  open,
  onClose,
  onKickComplete,
  partyId,
  participantUid,
  participantOwnerKey,
  currentThreshold,
  currentOwnerCount,
}: KickDialogProps) => {
  const [newThreshold, setNewThreshold] = useState(1);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<KickStatusResponse | null>(null);
  const { showSnackbar } = useSnackbar();
  // Local owner-key state. Initialized from the prop, but kept fresh by
  // polling /decentralized-parties while the dialog is open and the key is
  // still unknown — covers the cold-cache case where server-side resolution
  // finishes after App.tsx fetched its initial parties snapshot.
  const [resolvedOwnerKey, setResolvedOwnerKey] = useState<string | undefined>(
    participantOwnerKey,
  );

  useEffect(() => {
    setResolvedOwnerKey(participantOwnerKey);
  }, [participantOwnerKey]);

  useEffect(() => {
    if (!open || resolvedOwnerKey) return;
    const partyPrefix = partyId.split("::")[0];
    if (!partyPrefix) return;
    let cancelled = false;
    // On the first poll, force a server-side refresh — the cached
    // `/decentralized-parties` response can be missing the owner_key if
    // the previous resolve happened while the participant being kicked
    // was offline. Force=true triggers a fresh peer-Noise round-trip plus
    // the topology-derived fallback so the next poll usually has the key.
    let firstFetch = true;
    const fetchOwnerKey = async () => {
      try {
        const params = new URLSearchParams({ prefix: partyPrefix });
        if (firstFetch) {
          params.set("refresh", "true");
          firstFetch = false;
        }
        const res = await authenticatedFetch(
          `${API_BASE}/decentralized-parties?${params}`,
        );
        if (!res.ok) return;
        const data: DecentralizedPartiesResponse = await res.json();
        if (cancelled) return;
        const found = data.parties
          .find((p) => p.party_id === partyId)
          ?.participants.find((p) => p.participant_uid === participantUid)
          ?.owner_key;
        if (found) setResolvedOwnerKey(found);
      } catch {
        // Ignore polling errors
      }
    };
    fetchOwnerKey();
    const interval = window.setInterval(fetchOwnerKey, 2000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [open, resolvedOwnerKey, partyId, participantUid]);

  // Calculate suggested threshold when owner count changes
  const remainingOwners = currentOwnerCount - 1;
  const suggestedThreshold = Math.max(1, Math.ceil(remainingOwners / 2));

  useEffect(() => {
    // Reset threshold to suggested value when dialog opens with new data
    setNewThreshold(Math.min(currentThreshold, remainingOwners) || suggestedThreshold);
  }, [currentThreshold, remainingOwners, suggestedThreshold]);

  useEffect(() => {
    if (!open) {
      setError(null);
      setStatus(null);
      setLoading(false);
    }
  }, [open]);

  const pollStatus = useCallback(async () => {
    try {
      const res = await authenticatedFetch(`${API_BASE}/kick/status`);
      if (res.ok) {
        const data: KickStatusResponse = await res.json();
        if (data.status === "cancelled") {
          showSnackbar("Kick workflow cancelled");
          onClose();
          return;
        }
        setStatus(data);
        if (data.status !== "inprogress") {
          setLoading(false);
          if (data.status === "completed") {
            onKickComplete();
          }
        }
      }
    } catch {
      // Ignore polling errors
    }
  }, [onKickComplete, onClose, showSnackbar]);

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

  const handleKick = async () => {
    setLoading(true);
    setError(null);

    const request: KickRequest = {
      decentralized_party_id: partyId,
      participant_id: participantUid,
      new_threshold: newThreshold,
      previous_threshold: currentThreshold,
    };

    try {
      const res = await authenticatedFetch(`${API_BASE}/kick`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });

      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || "Failed to start kick workflow");
      }

      showSnackbar("Kick workflow started — follow progress in the feed");
      onClose();
      // Jump to the Approvals feed so the user lands on the run they
      // just started (refresh + navigate). Without this the dialog closes
      // back to the party detail and the in-flight kick is easy to miss.
      onKickComplete();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Unknown error");
      setLoading(false);
    }
  };

  const [cancelling, setCancelling] = useState(false);
  const handleCancelWorkflow = async () => {
    setCancelling(true);
    try {
      const res = await authenticatedFetch(`${API_BASE}/kick/cancel`, {
        method: "POST",
      });
      if (res.ok) {
        showSnackbar("Kick workflow cancelled");
        onClose();
      } else {
        const data = await res.json().catch(() => ({}));
        setError(data.error || "Failed to cancel workflow");
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to cancel workflow");
    } finally {
      setCancelling(false);
    }
  };

  const handleClose = () => {
    if (!loading) {
      onClose();
    }
  };

  return (
    <Dialog open={open} onClose={handleClose} maxWidth="sm" fullWidth>
      <DialogTitle>Kick Participant</DialogTitle>
      <DialogContent>
        <Box sx={{ display: "flex", flexDirection: "column", gap: 2, mt: 1 }}>
          <Typography variant="body2" color="text.secondary">
            Remove a participant from the decentralized party. This action
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
                  "The decentralized party the participant is being removed from. Pre-filled from the party you opened — not editable.",
                  "Help for Decentralized Party ID",
                ),
              },
            }}
          />

          <TextField
            label="Participant ID to Kick"
            value={participantUid}
            disabled
            fullWidth
            size="small"
            slotProps={{
              input: {
                endAdornment: fieldHelpAdornment(
                  "The participant being removed from the party. Pre-filled from the row you clicked Kick on.",
                  "Help for Participant ID to Kick",
                ),
              },
            }}
          />

          <TextField
            label="Namespace Fingerprint (Owner Key)"
            value={resolvedOwnerKey ?? ""}
            disabled
            fullWidth
            size="small"
            helperText={
              resolvedOwnerKey
                ? "The DNS owner key that will be removed"
                : "Owner key not yet known — waiting for cache resolution"
            }
            slotProps={{
              input: {
                endAdornment: fieldHelpAdornment(
                  "The participant's namespace fingerprint, looked up automatically from the participant via Noise or from Canton's topology. This is the key that gets removed from the decentralized namespace.",
                  "Help for Namespace Fingerprint",
                ),
              },
            }}
          />

          <TextField
            label="New Threshold"
            type="number"
            value={newThreshold}
            onChange={(e) => setNewThreshold(Math.max(1, parseInt(e.target.value) || 1))}
            fullWidth
            size="small"
            disabled={loading}
            slotProps={{
              htmlInput: { min: 1, max: remainingOwners },
              input: {
                endAdornment: fieldHelpAdornment(
                  "Number of remaining owners that must sign topology changes for this party after the kick. Must be between 1 and the number of owners left.",
                  "Help for New Threshold",
                ),
              },
            }}
            helperText={`Threshold after kick (suggested: ${suggestedThreshold}, max: ${remainingOwners})`}
          />

          {error && (
            <Alert severity="error" onClose={() => setError(null)}>
              {error}
            </Alert>
          )}

          {status?.status === "inprogress" && (
            <Alert severity="info" icon={<CircularProgress size={20} />}>
              Kick workflow in progress... This may take a few minutes.
            </Alert>
          )}

          {status?.status === "completed" && (
            <Alert severity="success">
              Participant has been successfully kicked from the party.
            </Alert>
          )}

          {status?.status === "failed" && (
            <Alert severity="error">
              Kick workflow failed: {status.error || "Unknown error"}
            </Alert>
          )}
        </Box>
      </DialogContent>
      <DialogActions>
        <Button onClick={handleClose} disabled={loading}>
          {status?.status === "completed" ||
          status?.status === "failed" ||
          status?.status === "inprogress"
            ? "Close"
            : "Cancel"}
        </Button>
        {status?.status === "inprogress" && (
          <Button
            onClick={handleCancelWorkflow}
            variant="outlined"
            color="error"
            disabled={cancelling}
            startIcon={cancelling ? <CircularProgress size={16} /> : undefined}
          >
            {cancelling ? "Cancelling…" : "Cancel Workflow"}
          </Button>
        )}
        {!status?.status ||
        status.status === "idle" ||
        status.status === "failed" ? (
          <Button
            onClick={handleKick}
            variant="contained"
            color="error"
            disabled={loading || newThreshold < 1 || newThreshold > remainingOwners || !resolvedOwnerKey}
          >
            {loading ? <CircularProgress size={20} /> : "Kick Participant"}
          </Button>
        ) : null}
      </DialogActions>
    </Dialog>
  );
};
