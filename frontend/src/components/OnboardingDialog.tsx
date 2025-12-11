import { useState, useEffect } from "react";
import {
  Dialog,
  DialogTitle,
  DialogContent,
  DialogActions,
  Button,
  Typography,
  CircularProgress,
  Alert,
  Box,
} from "@mui/material";
import { API_BASE } from "../constants";
import type { OnboardingStatusResponse } from "../types";

interface OnboardingDialogProps {
  open: boolean;
  onClose: () => void;
}

export const OnboardingDialog = ({ open, onClose }: OnboardingDialogProps) => {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<OnboardingStatusResponse | null>(null);

  useEffect(() => {
    if (!open) {
      setError(null);
      setStatus(null);
      setLoading(false);
    }
  }, [open]);

  useEffect(() => {
    let interval: number | undefined;

    if (status?.status === "inprogress") {
      interval = window.setInterval(async () => {
        try {
          const res = await fetch(`${API_BASE}/onboarding/status`);
          if (res.ok) {
            const data: OnboardingStatusResponse = await res.json();
            setStatus(data);
            if (data.status !== "inprogress") {
              setLoading(false);
            }
          }
        } catch {
          // Ignore polling errors
        }
      }, 2000);
    }

    return () => {
      if (interval) clearInterval(interval);
    };
  }, [status?.status]);

  const handleStart = async () => {
    setLoading(true);
    setError(null);

    try {
      const res = await fetch(`${API_BASE}/onboarding`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
      });

      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || "Failed to start onboarding workflow");
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
      <DialogTitle>Create Decentralized Party</DialogTitle>
      <DialogContent>
        <Box sx={{ display: "flex", flexDirection: "column", gap: 2, mt: 1 }}>
          <Typography variant="body2" color="text.secondary">
            Start the onboarding workflow to create a new decentralized party.
            This will coordinate with other participants to establish the party
            topology and namespace definition.
          </Typography>

          {error && <Alert severity="error">{error}</Alert>}

          {status?.status === "inprogress" && (
            <Alert severity="info" icon={<CircularProgress size={20} />}>
              Onboarding workflow in progress... This may take a few minutes.
            </Alert>
          )}

          {status?.status === "completed" && (
            <Alert severity="success">
              Decentralized party has been successfully created!
            </Alert>
          )}

          {status?.status === "failed" && (
            <Alert severity="error">
              Onboarding workflow failed: {status.error || "Unknown error"}
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
            onClick={handleStart}
            variant="contained"
            color="primary"
            disabled={loading}
          >
            {loading ? <CircularProgress size={20} /> : "Start Onboarding"}
          </Button>
        ) : null}
      </DialogActions>
    </Dialog>
  );
};
