import { useState } from "react";
import {
  Alert,
  Box,
  Button,
  CircularProgress,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  TextField,
  Typography,
} from "@mui/material";
import { API_BASE } from "../constants";
import { authenticatedFetch } from "../api";
import { useSnackbar } from "../contexts";
import type { RotateKeyRequest } from "../types";

interface RotateKeyDialogProps {
  open: boolean;
  onClose: () => void;
  onComplete: () => void;
  partyId: string;
  memberCount: number;
}

export const RotateKeyDialog = ({
  open,
  onClose,
  onComplete,
  partyId,
  memberCount,
}: RotateKeyDialogProps) => {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { showSnackbar } = useSnackbar();

  // The dialog stays mounted while closed, so it starts clean next time.
  const close = () => {
    setError(null);
    setLoading(false);
    onClose();
  };

  const handleSubmit = async () => {
    setLoading(true);
    setError(null);
    const request: RotateKeyRequest = { decentralized_party_id: partyId };
    try {
      const res = await authenticatedFetch(`${API_BASE}/rotate-key`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || "Failed to start the key rotation");
      }
      showSnackbar("Key rotation started — follow progress in the feed");
      close();
      // Jump to the feed so the user lands on the run they just started.
      onComplete();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Unknown error");
      setLoading(false);
    }
  };

  const handleClose = () => {
    if (!loading) {
      close();
    }
  };

  const others = memberCount - 1;

  return (
    <Dialog open={open} onClose={handleClose} maxWidth="sm" fullWidth>
      <DialogTitle>Rotate My Daml Key</DialogTitle>
      <DialogContent>
        <Box sx={{ display: "flex", flexDirection: "column", gap: 2, mt: 1 }}>
          <Typography variant="body2" color="text.secondary">
            This node gets a new Daml signing key for the party from its vault.
            The new key replaces this node's current key in the party's key
            list.
          </Typography>
          <Typography variant="body2" color="text.secondary">
            The participant must keep its keys in KMS. DecMan refuses the
            rotation otherwise.
          </Typography>
          <Typography variant="body2" color="text.secondary">
            {others === 1
              ? "The other member must sign the change."
              : `The other ${others} members must sign the change.`}{" "}
            The members, owners, and thresholds stay the same. The old key
            stays in the vault.
          </Typography>

          <TextField
            label="Decentralized Party ID"
            value={partyId}
            disabled
            fullWidth
            size="small"
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
          onClick={handleSubmit}
          variant="contained"
          disabled={loading || memberCount < 2}
        >
          {loading ? <CircularProgress size={20} /> : "Rotate Key"}
        </Button>
      </DialogActions>
    </Dialog>
  );
};
