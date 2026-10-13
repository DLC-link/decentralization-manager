import { useRef, useState } from "react";
import {
  Alert,
  Box,
  Button,
  CircularProgress,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  IconButton,
  Tooltip,
  Typography,
} from "@mui/material";
import ArrowBackIcon from "@mui/icons-material/ArrowBack";
import DeleteSweepIcon from "@mui/icons-material/DeleteSweep";
import VisibilityOffIcon from "@mui/icons-material/VisibilityOff";
import { CopyableText } from "./CopyableText";
import { StatCard } from "./StatCard";
import { ADMIN_ACCESS, API_BASE } from "../constants";
import { authenticatedFetch } from "../api";
import { useSnackbar } from "../contexts";
import type {
  FormerParty,
  PurgeFormerPartyRequest,
  PurgePreview,
  PurgeStatus,
} from "../types";

const STATUS_LABEL: Record<PurgeStatus, string> = {
  none: "Not started",
  running: "Running",
  succeeded: "Succeeded",
  failed: "Failed",
};

const formatTime = (epochSeconds: number) =>
  new Date(epochSeconds * 1000).toLocaleString(undefined, {
    year: "numeric",
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });

const contracts = (n: number) => `${n} ${n === 1 ? "contract" : "contracts"}`;

const StatusAlert = ({ party }: { party: FormerParty }) => {
  switch (party.purge_status) {
    case "none":
      return (
        <Alert severity="warning">
          This node no longer hosts this party. It still holds the contracts it
          had when it was removed. Nothing archives them on this node any more,
          so they go stale. Purge them to clean up.
        </Alert>
      );
    case "running":
      return (
        <Alert severity="info" icon={<CircularProgress size={20} />}>
          Purge in progress. The participant is off the synchronizer until it
          finishes.
        </Alert>
      );
    case "succeeded":
      return (
        <Alert severity="success">
          {`Purged ${contracts(party.purged_count ?? 0)}. Kept ${party.kept_count ?? 0} that another party on this node is a stakeholder of.`}
        </Alert>
      );
    case "failed":
      return (
        <Alert severity="error">
          {`The last purge failed: ${party.purge_error || "Unknown error"}`}
        </Alert>
      );
  }
};

interface FormerPartyDetailProps {
  party: FormerParty;
  /** Back to the list; also called once the party is dismissed. */
  onBack: () => void;
  /** Re-read the former parties, after a dismiss. */
  onChanged: () => void;
  /** The server accepted the purge, so the party now counts as running. */
  onPurgeStarted: () => void;
}

/**
 * A party this node hosted once and no longer hosts, with the purge of the
 * contracts it still holds for it.
 */
export const FormerPartyDetail = ({
  party,
  onBack,
  onChanged,
  onPurgeStarted,
}: FormerPartyDetailProps) => {
  const { showSnackbar } = useSnackbar();
  const [purgeOpen, setPurgeOpen] = useState(false);
  const [preview, setPreview] = useState<PurgePreview | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [posting, setPosting] = useState(false);
  const [dismissing, setDismissing] = useState(false);
  // Each opening of the dialog counts afresh; a late count from an earlier
  // opening must not land in a later one.
  const previewSeq = useRef(0);

  const prefix = party.party_id.split("::")[0];
  const status = party.purge_status;
  const nothingToPurge = preview?.purge_count === 0;

  const openPurge = async () => {
    const seq = ++previewSeq.current;
    setPreview(null);
    setPreviewError(null);
    setPurgeOpen(true);
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/former-parties/purge-preview?party_id=${encodeURIComponent(party.party_id)}`,
      );
      const data = await res.json().catch(() => ({}));
      if (seq !== previewSeq.current) return;
      if (res.ok) {
        setPreview(data as PurgePreview);
      } else {
        setPreviewError(data.error || "Failed to count the contracts");
      }
    } catch (e) {
      if (seq !== previewSeq.current) return;
      setPreviewError(
        e instanceof Error ? e.message : "Failed to count the contracts",
      );
    }
  };

  const handlePurge = async () => {
    setPosting(true);
    try {
      const body: PurgeFormerPartyRequest = { party_id: party.party_id };
      const res = await authenticatedFetch(`${API_BASE}/former-parties/purge`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      if (res.ok) {
        showSnackbar("Purge started");
        setPurgeOpen(false);
        onPurgeStarted();
      } else {
        const data = await res.json().catch(() => ({}));
        showSnackbar(data.error || "Failed to start the purge", "error");
      }
    } catch (e) {
      showSnackbar(
        e instanceof Error ? e.message : "Failed to start the purge",
        "error",
      );
    } finally {
      setPosting(false);
    }
  };

  const handleDismiss = async () => {
    setDismissing(true);
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/former-parties?party_id=${encodeURIComponent(party.party_id)}`,
        { method: "DELETE" },
      );
      if (res.ok) {
        showSnackbar("Dismissed");
        onChanged();
        onBack();
      } else {
        const data = await res.json().catch(() => ({}));
        showSnackbar(data.error || "Failed to dismiss the party", "error");
      }
    } catch (e) {
      showSnackbar(
        e instanceof Error ? e.message : "Failed to dismiss the party",
        "error",
      );
    } finally {
      setDismissing(false);
    }
  };

  return (
    <Box
      sx={{
        px: "var(--content-pad)",
        pt: 2,
        pb: 4,
        display: "flex",
        flexDirection: "column",
        gap: 2,
      }}
    >
      <Box
        sx={{
          display: "flex",
          alignItems: "flex-start",
          gap: 1.5,
          flexWrap: "wrap",
        }}
      >
        <Tooltip title="Back to parties">
          <IconButton onClick={onBack} sx={{ mt: 0.25, ml: "-8px" }}>
            <ArrowBackIcon />
          </IconButton>
        </Tooltip>
        <Box sx={{ flex: 1, minWidth: 240 }}>
          <Typography
            sx={{ fontSize: 24, fontWeight: 600, lineHeight: 1.2 }}
            data-testid="former-party-title"
          >
            {prefix}
          </Typography>
          <Box sx={{ mt: 0.25 }}>
            <CopyableText
              text={party.party_id}
              truncate={{ start: prefix.length + 12, end: 12 }}
              variant="body2"
            />
          </Box>
        </Box>
        <Box sx={{ display: "flex", gap: 1, flexWrap: "wrap" }}>
          {(status === "none" || status === "failed") && (
            <Button
              variant="contained"
              color="error"
              size="small"
              startIcon={<DeleteSweepIcon fontSize="small" />}
              onClick={openPurge}
              disabled={!ADMIN_ACCESS}
            >
              {status === "failed" ? "Retry purge" : "Purge"}
            </Button>
          )}
          {status === "succeeded" && (
            <Button
              variant="outlined"
              size="small"
              color="inherit"
              startIcon={
                dismissing ? (
                  <CircularProgress size={16} />
                ) : (
                  <VisibilityOffIcon fontSize="small" />
                )
              }
              onClick={handleDismiss}
              disabled={!ADMIN_ACCESS || dismissing}
              sx={{ color: "text.secondary", borderColor: "divider" }}
            >
              Dismiss
            </Button>
          )}
        </Box>
      </Box>

      <Box sx={{ display: "flex", gap: 1.5, flexWrap: "wrap" }}>
        <StatCard
          label="Removed"
          value={formatTime(party.removed_at)}
          helpText="When the hosting change that dropped this node took effect."
        />
        <StatCard
          label="Detected"
          value={formatTime(party.detected_at)}
          helpText="When this node noticed it no longer hosts the party."
        />
        <StatCard label="Purge" value={STATUS_LABEL[status]} />
      </Box>

      <StatusAlert party={party} />

      <Dialog
        open={purgeOpen}
        onClose={() => !posting && setPurgeOpen(false)}
        maxWidth="sm"
        fullWidth
      >
        <DialogTitle>Purge leftover contracts</DialogTitle>
        <DialogContent>
          <Box sx={{ display: "flex", flexDirection: "column", gap: 2 }}>
            {previewError ? (
              <Alert severity="error">{previewError}</Alert>
            ) : !preview ? (
              <Box sx={{ display: "flex", alignItems: "center", gap: 1.5 }}>
                <CircularProgress size={18} />
                <Typography variant="body2" color="text.secondary">
                  Counting contracts…
                </Typography>
              </Box>
            ) : nothingToPurge ? (
              <Typography variant="body2">
                Nothing to purge. Every contract of this party is shared with a
                party this node still hosts.
              </Typography>
            ) : (
              <>
                <Typography variant="body2">
                  {`This removes ${contracts(preview.purge_count)} of ${prefix} from this node. It keeps ${contracts(preview.keep_count)} that another party on this node is a stakeholder of.`}
                </Typography>
                <Alert severity="warning">
                  The participant leaves the synchronizer while the purge runs,
                  usually for about a minute. Every party this node hosts is
                  offline for that time. This cannot be undone.
                </Alert>
              </>
            )}
          </Box>
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setPurgeOpen(false)} disabled={posting}>
            Cancel
          </Button>
          <Button
            variant="contained"
            color={nothingToPurge ? "primary" : "error"}
            onClick={handlePurge}
            disabled={!preview || posting}
            startIcon={posting ? <CircularProgress size={16} /> : undefined}
          >
            {!preview
              ? "Purge"
              : nothingToPurge
                ? "Mark as purged"
                : `Purge ${contracts(preview.purge_count)}`}
          </Button>
        </DialogActions>
      </Dialog>
    </Box>
  );
};
