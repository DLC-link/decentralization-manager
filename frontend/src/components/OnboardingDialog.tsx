import { useState, useEffect, useCallback } from "react";
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
  TextField,
  FormGroup,
  FormControlLabel,
  Checkbox,
  Divider,
} from "@mui/material";
import { API_BASE } from "../constants";
import type { OnboardingStatusResponse, Peer, NodeConfig } from "../types";

interface OnboardingDialogProps {
  open: boolean;
  onClose: () => void;
  onComplete: () => void;
}

export const OnboardingDialog = ({
  open,
  onClose,
  onComplete,
}: OnboardingDialogProps) => {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<OnboardingStatusResponse | null>(null);
  const [partyIdPrefix, setPartyIdPrefix] = useState("");
  const [peers, setPeers] = useState<Peer[]>([]);
  const [selfNodeId, setSelfNodeId] = useState<string | null>(null);
  const [selectedPeerIds, setSelectedPeerIds] = useState<Set<string>>(
    new Set(),
  );
  const [loadingPeers, setLoadingPeers] = useState(false);

  // Fetch peers when dialog opens
  useEffect(() => {
    if (open) {
      const fetchPeers = async () => {
        setLoadingPeers(true);
        try {
          const [networkRes, nodeRes] = await Promise.all([
            fetch(`${API_BASE}/network-config`),
            fetch(`${API_BASE}/node-config`),
          ]);
          if (networkRes.ok) {
            const data = await networkRes.json();
            setPeers(data.peers || []);
            // Select all peers by default
            const allPeerIds = new Set<string>(
              (data.peers || []).map((p: Peer) => p.participant_id),
            );
            setSelectedPeerIds(allPeerIds);
          }
          if (nodeRes.ok) {
            const nodeData: NodeConfig = await nodeRes.json();
            setSelfNodeId(nodeData.node.participant_id);
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

  useEffect(() => {
    if (!open) {
      setError(null);
      setStatus(null);
      setLoading(false);
      setPartyIdPrefix("");
      setSelectedPeerIds(new Set());
    }
  }, [open]);

  const togglePeer = (peerId: string) => {
    setSelectedPeerIds((prev) => {
      const newSet = new Set(prev);
      if (newSet.has(peerId)) {
        newSet.delete(peerId);
      } else {
        newSet.add(peerId);
      }
      return newSet;
    });
  };

  // Filter out self from peer list (compare prefix of participant_id with selfNodeId)
  const selectablePeers = peers.filter(
    (p) => p.participant_id.split("::")[0] !== selfNodeId,
  );

  const pollStatus = useCallback(async () => {
    try {
      const res = await fetch(`${API_BASE}/onboarding/status`);
      if (res.ok) {
        const data: OnboardingStatusResponse = await res.json();
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
  }, [onComplete]);

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

  const handleStart = async () => {
    if (!partyIdPrefix.trim()) {
      setError("Party ID prefix is required");
      return;
    }

    if (selectedPeerIds.size === 0) {
      setError("At least one peer must be selected");
      return;
    }

    setLoading(true);
    setError(null);

    try {
      const res = await fetch(`${API_BASE}/onboarding`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          party_id_prefix: partyIdPrefix.trim(),
          peer_ids: Array.from(selectedPeerIds),
        }),
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

          <TextField
            label="Party ID Prefix"
            value={partyIdPrefix}
            onChange={(e) => setPartyIdPrefix(e.target.value)}
            placeholder="e.g., my-network"
            fullWidth
            disabled={loading || status?.status === "inprogress"}
            helperText="A unique identifier prefix for the decentralized party"
          />

          <Divider />

          <Box>
            <Typography variant="subtitle2" sx={{ mb: 1 }}>
              Select Peers to Invite
            </Typography>
            {loadingPeers ? (
              <Box sx={{ display: "flex", justifyContent: "center", py: 2 }}>
                <CircularProgress size={24} />
              </Box>
            ) : selectablePeers.length === 0 ? (
              <Typography variant="body2" color="text.secondary">
                No peers configured. Add peers in the Network Configuration
                first.
              </Typography>
            ) : (
              <FormGroup>
                {selectablePeers.map((peer) => (
                  <FormControlLabel
                    key={peer.participant_id}
                    control={
                      <Checkbox
                        checked={selectedPeerIds.has(peer.participant_id)}
                        onChange={() => togglePeer(peer.participant_id)}
                        disabled={loading || status?.status === "inprogress"}
                      />
                    }
                    label={
                      <Box>
                        <Typography variant="body2">
                          {peer.name || peer.participant_id}
                        </Typography>
                        <Typography variant="caption" color="text.secondary">
                          {peer.address}:{peer.port}
                        </Typography>
                      </Box>
                    }
                  />
                ))}
              </FormGroup>
            )}
          </Box>

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
            disabled={
              loading || !partyIdPrefix.trim() || selectedPeerIds.size === 0
            }
          >
            {loading ? <CircularProgress size={20} /> : "Start Onboarding"}
          </Button>
        ) : null}
      </DialogActions>
    </Dialog>
  );
};
