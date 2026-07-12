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
import { authenticatedFetch } from "../api";
import { useSnackbar } from "../contexts";
import { fieldHelpAdornment } from "./FieldHelp";
import type { WorkflowStatusResponse, Peer, NodeConfig } from "../types";

interface ExternalPartyDialogProps {
  open: boolean;
  onClose: () => void;
  onCreated: () => void;
}

// Mirrors the server-side party-hint rule (canton_id.rs `validate_party_id_prefix`):
// the hint becomes the party-id identifier (`<hint>::<namespace>`), so only ASCII
// letters/digits/'-'/'_' are allowed, it must start with a letter, and be ≤180 chars.
const MAX_PARTY_HINT_LEN = 180;
const partyHintError = (hint: string): string | null => {
  if (hint.length === 0) return null; // empty handled by the disabled state
  if (hint.length > MAX_PARTY_HINT_LEN)
    return `Must be at most ${MAX_PARTY_HINT_LEN} characters`;
  if (!/^[A-Za-z]/.test(hint)) return "Must start with a letter (a–z, A–Z)";
  if (!/^[A-Za-z0-9_-]+$/.test(hint))
    return "Only letters, digits, '-' and '_' are allowed";
  return null;
};

export const ExternalPartyDialog = ({
  open,
  onClose,
  onCreated,
}: ExternalPartyDialogProps) => {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [meshErrors, setMeshErrors] = useState<
    Array<{
      from: string;
      to: string;
      // `kind` is optional for backwards compatibility with older backends
      // that didn't tag the failure mode.
      kind?: "unreachable_from_coordinator" | "mesh_hole";
    }> | null
  >(null);
  const [status, setStatus] = useState<WorkflowStatusResponse | null>(null);
  const [partyHint, setPartyHint] = useState("");
  const [peers, setPeers] = useState<Peer[]>([]);
  const [selfNodeId, setSelfNodeId] = useState<string | null>(null);
  const [selectedPeerIds, setSelectedPeerIds] = useState<Set<string>>(
    new Set(),
  );
  const [loadingPeers, setLoadingPeers] = useState(false);
  // Confirmation threshold. Defaults to a majority of hosts (`ceil(owners / 2)`)
  // — a sensible fault-tolerant default, recomputed as the selected hosting-peer
  // set changes, until the operator edits it. The dialog always sends an explicit
  // value; the server's own fallback for an unset threshold is all hosts.
  const [threshold, setThreshold] = useState(1);
  const [thresholdTouched, setThresholdTouched] = useState(false);
  const { showSnackbar } = useSnackbar();

  // Owners = the selected hosting peers plus this node (always a host of a
  // party it onboards). The default threshold is the majority of that count.
  const ownerCount = selectedPeerIds.size + 1;
  const defaultThreshold = Math.max(1, Math.ceil(ownerCount / 2));

  // Keep the field on the computed default until the operator touches it, so
  // it always shows a sensible value as peers are checked/unchecked.
  useEffect(() => {
    if (!thresholdTouched) setThreshold(defaultThreshold);
  }, [defaultThreshold, thresholdTouched]);

  // Fetch peers when dialog opens
  useEffect(() => {
    if (open) {
      const fetchPeers = async () => {
        setLoadingPeers(true);
        try {
          const [networkRes, nodeRes] = await Promise.all([
            authenticatedFetch(`${API_BASE}/network-config`),
            authenticatedFetch(`${API_BASE}/node-config`),
          ]);
          let self: string | null = null;
          if (nodeRes.ok) {
            const nodeData: NodeConfig = await nodeRes.json();
            self = nodeData.node.participant_id;
            setSelfNodeId(self);
          }
          if (networkRes.ok) {
            const data = await networkRes.json();
            const allPeers: Peer[] = data.peers || [];
            setPeers(allPeers);
            // Select all peers by default, excluding self
            const allPeerIds = new Set<string>(
              allPeers
                .filter((p) => p.participant_id !== self)
                .map((p) => p.participant_id),
            );
            setSelectedPeerIds(allPeerIds);
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
      setMeshErrors(null);
      setStatus(null);
      setLoading(false);
      setPartyHint("");
      setSelectedPeerIds(new Set());
      setThresholdTouched(false);
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

  // Filter out self from peer list (compare full canton ids).
  const selectablePeers = peers.filter((p) => p.participant_id !== selfNodeId);

  const pollStatus = useCallback(async () => {
    try {
      const res = await authenticatedFetch(`${API_BASE}/external-party/status`);
      if (res.ok) {
        const data: WorkflowStatusResponse = await res.json();
        setStatus(data);
        if (data.status !== "inprogress") {
          setLoading(false);
          if (data.status === "completed") {
            showSnackbar("External party created");
            onCreated();
          }
        }
      }
    } catch {
      // Ignore polling errors
    }
  }, [onCreated, showSnackbar]);

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

  const handleStart = async () => {
    const hint = partyHint.trim();
    if (!hint) {
      setError("Party hint is required");
      return;
    }
    const hintErr = partyHintError(hint);
    if (hintErr) {
      setError(`Party hint invalid: ${hintErr}`);
      return;
    }
    if (selectedPeerIds.size === 0) {
      setError("At least one hosting peer must be selected");
      return;
    }
    if (threshold < 1 || threshold > ownerCount) {
      setError(`Threshold must be between 1 and ${ownerCount}`);
      return;
    }

    setLoading(true);
    setError(null);
    setMeshErrors(null);

    try {
      const res = await authenticatedFetch(`${API_BASE}/external-party`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          party_hint: hint,
          hosting_peers: Array.from(selectedPeerIds),
          confirmation_threshold: threshold,
        }),
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        if (
          res.status === 422 &&
          Array.isArray(data.missing_edges) &&
          data.missing_edges.length > 0
        ) {
          setMeshErrors(data.missing_edges);
          setError(data.error || "Selected peers are not mutually connected");
          setLoading(false);
          return;
        }
        throw new Error(
          data.error || "Failed to start external-party onboarding",
        );
      }
      // 202 Accepted — the run is async; switch to polling its status.
      setStatus({ status: "inprogress" });
    } catch (err) {
      setError(err instanceof Error ? err.message : "Unknown error");
      setLoading(false);
    }
  };

  const handleClose = () => {
    if (!loading) onClose();
  };

  const peerName = (id: string) =>
    peers.find((p) => p.participant_id === id)?.name || id;

  // Split missing directed edges into the two distinct user-actions:
  //
  //  - `mesh_hole`  — peer `from` is reachable from the coordinator but does
  //    not have peer `to` in its config. Hint: "On <from>, add <to>".
  //  - `unreachable_from_coordinator` — coordinator can't query `to` at all
  //    (unknown / no key / unreachable / unparsable response). Hint: fix the
  //    coordinator's view of `to`, or `to` itself (it may be offline).
  //
  // Older backends won't set `kind`; treat that as `mesh_hole` so behavior
  // is identical to the pre-fix UI.
  const meshHoles = (() => {
    if (!meshErrors) return [] as Array<{ node: string; missing: string[] }>;
    const map = new Map<string, string[]>();
    for (const edge of meshErrors) {
      if (edge.kind && edge.kind !== "mesh_hole") continue;
      const list = map.get(edge.from) ?? [];
      list.push(edge.to);
      map.set(edge.from, list);
    }
    return Array.from(map.entries()).map(([node, missing]) => ({
      node,
      missing,
    }));
  })();
  const unreachablePeers = (() => {
    if (!meshErrors) return [] as string[];
    const set = new Set<string>();
    for (const edge of meshErrors) {
      if (edge.kind === "unreachable_from_coordinator") {
        set.add(edge.to);
      }
    }
    return Array.from(set);
  })();

  const hintErr = partyHintError(partyHint.trim());
  const inProgress = status?.status === "inprogress";
  const disableInputs = loading || inProgress;

  return (
    <Dialog open={open} onClose={handleClose} maxWidth="sm" fullWidth>
      <DialogTitle>Create External Party</DialogTitle>
      <DialogContent>
        <Box sx={{ display: "flex", flexDirection: "column", gap: 2, mt: 1 }}>
          <Typography variant="body2" color="text.secondary">
            Onboard a decentrally-hosted (multi-host) external party. The party's
            Ed25519 key is generated client-side and hosted with Confirmation
            permission across this node and the selected participants. Topology
            changes are authorized once the chosen number of hosts confirm.
          </Typography>

          <TextField
            label="Party Hint"
            value={partyHint}
            onChange={(e) => setPartyHint(e.target.value)}
            placeholder="e.g., alice"
            fullWidth
            disabled={disableInputs}
            error={!!hintErr}
            helperText={
              hintErr ?? "Becomes the identifier before '::' in the party id"
            }
            slotProps={{
              input: {
                endAdornment: fieldHelpAdornment(
                  "The human-readable hint for the external party. It shows up as the bit before '::' in the party id and must be unique on this node. Cannot be changed after creation.",
                  "Help for Party Hint",
                ),
              },
            }}
          />

          <Divider />

          <Box>
            <Typography variant="subtitle2" sx={{ mb: 1 }}>
              Select Hosting Peers
            </Typography>
            {loadingPeers ? (
              <Box sx={{ display: "flex", justifyContent: "center", py: 2 }}>
                <CircularProgress size={24} />
              </Box>
            ) : selectablePeers.length === 0 ? (
              <Typography variant="body2" color="text.secondary">
                No peers configured. A decentrally-hosted party needs at least
                one other host — add peers in the Network Configuration first.
              </Typography>
            ) : (
              <>
                <FormGroup>
                  {selectablePeers.map((peer) => (
                    <FormControlLabel
                      key={peer.participant_id}
                      control={
                        <Checkbox
                          checked={selectedPeerIds.has(peer.participant_id)}
                          onChange={() => togglePeer(peer.participant_id)}
                          disabled={disableInputs}
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
                {selectedPeerIds.size === 0 && (
                  <Typography variant="caption" color="error">
                    Select at least one hosting peer.
                  </Typography>
                )}
              </>
            )}
          </Box>

          <TextField
            label="Confirmation Threshold"
            type="number"
            value={threshold}
            onChange={(e) => {
              setThresholdTouched(true);
              setThreshold(Math.max(1, parseInt(e.target.value) || 1));
            }}
            fullWidth
            disabled={disableInputs}
            error={threshold < 1 || threshold > ownerCount}
            slotProps={{
              htmlInput: { min: 1, max: ownerCount },
              input: {
                endAdornment: fieldHelpAdornment(
                  "How many of the party's hosts must confirm a transaction (the party-to-participant confirmation threshold). Hosts are you plus the peers you select. Defaults to a majority; edit to override.",
                  "Help for Confirmation Threshold",
                ),
              },
            }}
            helperText={
              threshold > ownerCount
                ? `Must be at most ${ownerCount} (you + ${selectedPeerIds.size} peer${
                    selectedPeerIds.size === 1 ? "" : "s"
                  })`
                : `Of ${ownerCount} host${ownerCount === 1 ? "" : "s"} (you + ${
                    selectedPeerIds.size
                  } peer${
                    selectedPeerIds.size === 1 ? "" : "s"
                  }). Default: majority (${defaultThreshold}).`
            }
          />

          {error && !meshErrors && (
            <Alert severity="error" onClose={() => setError(null)}>
              {error}
            </Alert>
          )}

          {meshErrors && (
            <Alert severity="error">
              {unreachablePeers.length > 0 && (
                <Box sx={{ mb: meshHoles.length > 0 ? 2 : 0 }}>
                  <Typography variant="body2" sx={{ fontWeight: 600, mb: 1 }}>
                    Coordinator can't reach these peers:
                  </Typography>
                  <Box component="ul" sx={{ pl: 2.5, m: 0 }}>
                    {unreachablePeers.map((id) => (
                      <Typography
                        component="li"
                        variant="body2"
                        key={id}
                        color="text.secondary"
                      >
                        {peerName(id)}
                      </Typography>
                    ))}
                  </Box>
                  <Typography
                    variant="caption"
                    color="text.secondary"
                    sx={{ display: "block", mt: 0.5 }}
                  >
                    Fix the coordinator's network config for each, or check that
                    the peer is online.
                  </Typography>
                </Box>
              )}
              {meshHoles.length > 0 && (
                <>
                  <Typography variant="body2" sx={{ fontWeight: 600, mb: 1 }}>
                    Update network configs:
                  </Typography>
                  <Box sx={{ display: "flex", flexDirection: "column", gap: 1 }}>
                    {meshHoles.map(({ node, missing }, i) => (
                      <Box key={i}>
                        <Typography variant="body2">
                          On <strong>{peerName(node)}</strong>, add:
                        </Typography>
                        <Box component="ul" sx={{ pl: 2.5, m: 0 }}>
                          {missing.map((toId, j) => (
                            <Typography
                              component="li"
                              variant="body2"
                              key={j}
                              color="text.secondary"
                            >
                              {peerName(toId)}
                            </Typography>
                          ))}
                        </Box>
                      </Box>
                    ))}
                  </Box>
                </>
              )}
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ display: "block", mt: 1 }}
              >
                Then retry.
              </Typography>
            </Alert>
          )}

          {status?.status === "inprogress" && (
            <Alert severity="info" icon={<CircularProgress size={20} />}>
              Onboarding external party…
            </Alert>
          )}
          {status?.status === "completed" && (
            <Alert severity="success">External party created!</Alert>
          )}
          {status?.status === "failed" && (
            <Alert severity="error">
              Onboarding failed: {status.error || "Unknown error"}
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
        {(!status?.status ||
          status.status === "idle" ||
          status.status === "failed") && (
          <Button
            onClick={handleStart}
            variant="contained"
            color="primary"
            disabled={
              loading ||
              !partyHint.trim() ||
              !!hintErr ||
              selectedPeerIds.size === 0 ||
              threshold < 1 ||
              threshold > ownerCount
            }
          >
            {loading ? <CircularProgress size={20} /> : "Create"}
          </Button>
        )}
      </DialogActions>
    </Dialog>
  );
};
