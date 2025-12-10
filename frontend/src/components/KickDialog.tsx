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
} from "@mui/material";
import { API_BASE } from "../constants";
import type { KickRequest, KickStatusResponse } from "../types";

interface KickDialogProps {
  open: boolean;
  onClose: () => void;
  partyId: string;
  participantUid: string;
  ownerKey: string;
}

export const KickDialog = ({
  open,
  onClose,
  partyId,
  participantUid,
  ownerKey,
}: KickDialogProps) => {
  const [namespaceFingerprint, setNamespaceFingerprint] = useState(ownerKey);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<KickStatusResponse | null>(null);

  useEffect(() => {
    setNamespaceFingerprint(ownerKey);
  }, [ownerKey]);

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
          const res = await fetch(`${API_BASE}/kick/status`);
          if (res.ok) {
            const data: KickStatusResponse = await res.json();
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

  const handleKick = async () => {
    setLoading(true);
    setError(null);

    const request: KickRequest = {
      decentralized_party_id: partyId,
      participant_id: participantUid,
      namespace_fingerprint: namespaceFingerprint,
    };

    try {
      const res = await fetch(`${API_BASE}/kick`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });

      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || "Failed to start kick workflow");
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
          />

          <TextField
            label="Participant ID to Kick"
            value={participantUid}
            disabled
            fullWidth
            size="small"
          />

          <TextField
            label="Namespace Fingerprint (Owner Key)"
            value={namespaceFingerprint}
            onChange={(e) => setNamespaceFingerprint(e.target.value)}
            fullWidth
            size="small"
            disabled={loading}
            helperText="The namespace fingerprint (DNS owner key) to remove"
          />

          {error && <Alert severity="error">{error}</Alert>}

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
          {status?.status === "completed" || status?.status === "failed"
            ? "Close"
            : "Cancel"}
        </Button>
        {!status?.status ||
        status.status === "idle" ||
        status.status === "failed" ? (
          <Button
            onClick={handleKick}
            variant="contained"
            color="error"
            disabled={loading || !namespaceFingerprint}
          >
            {loading ? <CircularProgress size={20} /> : "Kick Participant"}
          </Button>
        ) : null}
      </DialogActions>
    </Dialog>
  );
};
