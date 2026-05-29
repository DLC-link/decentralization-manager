import { useEffect, useState } from "react";
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
import { fieldHelpAdornment } from "./FieldHelp";

interface GrantRightsDialogProps {
  open: boolean;
  onClose: () => void;
  onGranted: () => void;
  partyId: string;
}

export const GrantRightsDialog = ({
  open,
  onClose,
  onGranted,
  partyId,
}: GrantRightsDialogProps) => {
  const [clientId, setClientId] = useState("");
  const [clientSecret, setClientSecret] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Always wipe the secret when the dialog opens (or closes) so it never
  // carries across opens. Client_id is preserved so the operator doesn't
  // have to retype it on a wrong-secret retry.
  useEffect(() => {
    setClientSecret("");
    setError(null);
    setLoading(false);
    if (!open) {
      setClientId("");
    }
  }, [open]);

  const canSubmit =
    !loading && clientId.trim().length > 0 && clientSecret.length > 0;

  const handleSubmit = async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await authenticatedFetch(`${API_BASE}/auth/grant-rights`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          dec_party_id: partyId,
          admin_client_id: clientId.trim(),
          admin_client_secret: clientSecret,
        }),
      });
      if (res.ok) {
        setClientSecret("");
        onGranted();
        onClose();
      } else {
        const data = await res.json().catch(() => ({}));
        setClientSecret("");
        setError(data.error || "Failed to grant rights");
      }
    } catch (err) {
      setClientSecret("");
      setError(err instanceof Error ? err.message : "Unknown error");
    } finally {
      setLoading(false);
    }
  };

  const handleClose = () => {
    if (!loading) onClose();
  };

  return (
    <Dialog open={open} onClose={handleClose} maxWidth="xs" fullWidth>
      <DialogTitle>Grant Rights — Admin Credentials</DialogTitle>
      <DialogContent>
        <Box sx={{ display: "flex", flexDirection: "column", gap: 2, mt: 1 }}>
          <Typography variant="body2" color="text.secondary">
            Used once to grant <code>actAs</code> + <code>readAs</code> on the
            member and dec parties. Credentials are not stored — paste the
            Keycloak client whose service account has{" "}
            <code>ParticipantAdmin</code> on this participant (the same one
            your team uses in Yaak).
          </Typography>

          <TextField
            label="Client ID"
            value={clientId}
            onChange={(e) => setClientId(e.target.value)}
            size="small"
            fullWidth
            autoFocus
            disabled={loading}
            autoComplete="off"
            slotProps={{
              input: {
                endAdornment: fieldHelpAdornment(
                  "The Keycloak client ID whose service account has ParticipantAdmin rights on this participant. The same one your team uses in Yaak.",
                  "Help for Client ID",
                ),
              },
            }}
          />

          <TextField
            label="Client Secret"
            value={clientSecret}
            onChange={(e) => setClientSecret(e.target.value)}
            size="small"
            type="password"
            fullWidth
            disabled={loading}
            autoComplete="off"
            slotProps={{
              input: {
                endAdornment: fieldHelpAdornment(
                  "The secret that pairs with the client ID above. Used once to grant rights, then discarded — never stored.",
                  "Help for Client Secret",
                ),
              },
            }}
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
          color="primary"
          disabled={!canSubmit}
        >
          {loading ? <CircularProgress size={20} color="inherit" /> : "Grant"}
        </Button>
      </DialogActions>
    </Dialog>
  );
};
