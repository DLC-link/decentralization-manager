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
import type { ChangeThresholdRequest, ChangeThresholdStatusResponse } from "../types";

interface ChangeThresholdDialogProps {
  open: boolean;
  onClose: () => void;
  onComplete: () => void;
  partyId: string;
  currentThreshold: number;
  currentOwnerCount: number;
}

export const ChangeThresholdDialog = ({
  open,
  onClose,
  onComplete,
  partyId,
  currentThreshold,
  currentOwnerCount,
}: ChangeThresholdDialogProps) => {
  const [newThreshold, setNewThreshold] = useState(currentThreshold);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<ChangeThresholdStatusResponse | null>(
    null,
  );
  const { showSnackbar } = useSnackbar();

  useEffect(() => {
    // Seed with the current threshold each time the dialog opens with fresh
    // party data; the user edits from there.
    setNewThreshold(currentThreshold);
  }, [currentThreshold, open]);

  useEffect(() => {
    if (!open) {
      setError(null);
      setStatus(null);
      setLoading(false);
    }
  }, [open]);

  const pollStatus = useCallback(async () => {
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/change-threshold/status`,
      );
      if (res.ok) {
        const data: ChangeThresholdStatusResponse = await res.json();
        if (data.status === "cancelled") {
          showSnackbar("Change-threshold workflow cancelled");
          onClose();
          return;
        }
        setStatus(data);
        if (data.status !== "inprogress") {
          setLoading(false);
          if (data.status === "completed") {
            onComplete();
          }
        }
      }
    } catch {
      // Ignore polling errors
    }
  }, [onComplete, onClose, showSnackbar]);

  useEffect(() => {
    let interval: number | undefined;

    if (status?.status === "inprogress") {
      pollStatus();
      interval = window.setInterval(pollStatus, 2000);
    }

    return () => {
      if (interval) clearInterval(interval);
    };
  }, [status?.status, pollStatus]);

  const unchanged = newThreshold === currentThreshold;
  const outOfRange = newThreshold < 1 || newThreshold > currentOwnerCount;

  const handleSubmit = async () => {
    setLoading(true);
    setError(null);

    const request: ChangeThresholdRequest = {
      decentralized_party_id: partyId,
      new_threshold: newThreshold,
      previous_threshold: currentThreshold,
    };

    try {
      const res = await authenticatedFetch(`${API_BASE}/change-threshold`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });

      if (!res.ok) {
        const data = await res.json();
        throw new Error(
          data.error || "Failed to start change-threshold workflow",
        );
      }

      showSnackbar(
        "Change-threshold workflow started — follow progress in the feed",
      );
      onClose();
      // Jump to the feed so the user lands on the run they just started.
      onComplete();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Unknown error");
      setLoading(false);
    }
  };

  const [cancelling, setCancelling] = useState(false);
  const handleCancelWorkflow = async () => {
    setCancelling(true);
    try {
      const res = await authenticatedFetch(`${API_BASE}/change-threshold/cancel`, {
        method: "POST",
      });
      if (res.ok) {
        showSnackbar("Change-threshold workflow cancelled");
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
      <DialogTitle>Change Threshold</DialogTitle>
      <DialogContent>
        <Box sx={{ display: "flex", flexDirection: "column", gap: 2, mt: 1 }}>
          <Typography variant="body2" color="text.secondary">
            Change the signing threshold of this decentralized party — how many
            of its {currentOwnerCount} owners must sign topology changes.
            Membership is unchanged. This action requires coordination with the
            other participants.
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
                  "The decentralized party whose threshold is being changed. Pre-filled from the party you opened — not editable.",
                  "Help for Decentralized Party ID",
                ),
              },
            }}
          />

          <TextField
            label="Current Threshold"
            value={`${currentThreshold} of ${currentOwnerCount}`}
            disabled
            fullWidth
            size="small"
          />

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
            error={unchanged || outOfRange}
            slotProps={{
              htmlInput: { min: 1, max: currentOwnerCount },
              input: {
                endAdornment: fieldHelpAdornment(
                  "Number of owners that must sign topology changes for this party after the change. Must be between 1 and the number of owners, and different from the current threshold.",
                  "Help for New Threshold",
                ),
              },
            }}
            helperText={
              unchanged
                ? "Pick a value different from the current threshold"
                : outOfRange
                  ? `Must be between 1 and ${currentOwnerCount}`
                  : `New threshold (max: ${currentOwnerCount})`
            }
          />

          {error && (
            <Alert severity="error" onClose={() => setError(null)}>
              {error}
            </Alert>
          )}

          {status?.status === "inprogress" && (
            <Alert severity="info" icon={<CircularProgress size={20} />}>
              Change-threshold workflow in progress... This may take a few
              minutes.
            </Alert>
          )}

          {status?.status === "completed" && (
            <Alert severity="success">
              Party threshold has been successfully changed.
            </Alert>
          )}

          {status?.status === "failed" && (
            <Alert severity="error">
              Change-threshold workflow failed: {status.error || "Unknown error"}
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
            onClick={handleSubmit}
            variant="contained"
            disabled={loading || unchanged || outOfRange}
          >
            {loading ? <CircularProgress size={20} /> : "Change Threshold"}
          </Button>
        ) : null}
      </DialogActions>
    </Dialog>
  );
};
