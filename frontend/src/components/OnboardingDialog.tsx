import { useState, useEffect, useCallback } from "react";
import {
  Alert,
  Box,
  Button,
  CircularProgress,
  Dialog,
  InputAdornment,
  TextField,
  Typography,
} from "@mui/material";
import CheckIcon from "@mui/icons-material/Check";
import SearchIcon from "@mui/icons-material/Search";
import { API_BASE } from "../constants";
import { authenticatedFetch } from "../api";
import { useSnackbar } from "../contexts";
import { fieldHelpAdornment } from "./FieldHelp";
import type { OnboardingStatusResponse, Peer, NodeConfig } from "../types";

interface OnboardingDialogProps {
  open: boolean;
  onClose: () => void;
  onComplete: () => void;
}

// Mirrors the server-side `validate_party_id_prefix` (canton_id.rs): the
// prefix becomes the Canton party-id identifier (`<prefix>::<namespace>`), so
// only ASCII letters/digits/'-'/'_' are allowed, it must start with a letter,
// and be at most 180 chars. Returns an error message, or null if valid.
const MAX_PARTY_ID_PREFIX_LEN = 180;
const partyPrefixError = (prefix: string): string | null => {
  if (prefix.length === 0) return null; // empty handled by the disabled state
  if (prefix.length > MAX_PARTY_ID_PREFIX_LEN)
    return `Must be at most ${MAX_PARTY_ID_PREFIX_LEN} characters`;
  if (!/^[A-Za-z]/.test(prefix)) return "Must start with a letter (a–z, A–Z)";
  if (!/^[A-Za-z0-9_-]+$/.test(prefix))
    return "Only letters, digits, “-” and “_” are allowed";
  return null;
};

export const OnboardingDialog = ({
  open,
  onClose,
  onComplete,
}: OnboardingDialogProps) => {
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
  const [status, setStatus] = useState<OnboardingStatusResponse | null>(null);
  const [partyIdPrefix, setPartyIdPrefix] = useState("");
  const [peers, setPeers] = useState<Peer[]>([]);
  const [selfNodeId, setSelfNodeId] = useState<string | null>(null);
  const [selectedPeerIds, setSelectedPeerIds] = useState<Set<string>>(
    new Set(),
  );
  const [filter, setFilter] = useState("");
  const [loadingPeers, setLoadingPeers] = useState(false);
  const { showSnackbar } = useSnackbar();

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
          if (nodeRes.ok) {
            const nodeData: NodeConfig = await nodeRes.json();
            setSelfNodeId(nodeData.node.participant_id);
          }
          if (networkRes.ok) {
            const data = await networkRes.json();
            const allPeers: Peer[] = data.peers || [];
            setPeers(allPeers);
            // Nothing is preselected — the operator opts peers in explicitly.
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
      setPartyIdPrefix("");
      setSelectedPeerIds(new Set());
      setFilter("");
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
  const selectablePeers = peers.filter(
    (p) => p.participant_id !== selfNodeId,
  );

  // Apply the free-text filter (name or address) to the selectable peers.
  const visiblePeers = selectablePeers.filter((p) => {
    const q = filter.trim().toLowerCase();
    if (!q) return true;
    return (
      (p.name || p.participant_id).toLowerCase().includes(q) ||
      `${p.address}:${p.port}`.toLowerCase().includes(q)
    );
  });

  const allVisibleSelected =
    visiblePeers.length > 0 &&
    visiblePeers.every((p) => selectedPeerIds.has(p.participant_id));

  const toggleAllVisible = () => {
    setSelectedPeerIds((prev) => {
      const next = new Set(prev);
      if (allVisibleSelected) {
        visiblePeers.forEach((p) => next.delete(p.participant_id));
      } else {
        visiblePeers.forEach((p) => next.add(p.participant_id));
      }
      return next;
    });
  };

  // The filter only earns its space once the list is long enough to scan.
  const showFilter = selectablePeers.length > 6;

  const pollStatus = useCallback(async () => {
    try {
      const res = await authenticatedFetch(`${API_BASE}/onboarding/status`);
      if (res.ok) {
        const data: OnboardingStatusResponse = await res.json();
        if (data.status === "cancelled") {
          showSnackbar("Onboarding workflow cancelled");
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

    const prefixErr = partyPrefixError(partyIdPrefix.trim());
    if (prefixErr) {
      setError(`Party ID prefix invalid: ${prefixErr}`);
      return;
    }

    if (selectedPeerIds.size === 0) {
      setError("At least one peer must be selected");
      return;
    }

    setLoading(true);
    setError(null);
    setMeshErrors(null);

    try {
      const res = await authenticatedFetch(`${API_BASE}/onboarding`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          party_id_prefix: partyIdPrefix.trim(),
          peer_ids: Array.from(selectedPeerIds),
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
        throw new Error(data.error || "Failed to start onboarding workflow");
      }

      showSnackbar("Onboarding workflow started — follow progress in the feed");
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Unknown error");
      setLoading(false);
    }
  };

  const [cancelling, setCancelling] = useState(false);
  const handleCancelWorkflow = async () => {
    setCancelling(true);
    try {
      const res = await authenticatedFetch(`${API_BASE}/onboarding/cancel`, {
        method: "POST",
      });
      if (res.ok) {
        showSnackbar("Onboarding workflow cancelled");
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

  const controlsDisabled = loading || status?.status === "inprogress";
  const isStartState =
    !status?.status || status.status === "idle" || status.status === "failed";
  const prefixValid =
    !!partyIdPrefix.trim() && !partyPrefixError(partyIdPrefix.trim());
  const footerNote =
    selectedPeerIds.size === 0
      ? "Select at least one peer"
      : !prefixValid
        ? "Enter a party ID prefix"
        : `${selectedPeerIds.size} ${
            selectedPeerIds.size === 1 ? "peer" : "peers"
          } selected`;

  return (
    <Dialog
      open={open}
      onClose={handleClose}
      maxWidth={false}
      slotProps={{
        paper: {
          sx: {
            width: 480,
            maxWidth: "calc(100% - 32px)",
            maxHeight: "min(90vh, 720px)",
            borderRadius: "12px",
            border: "1px solid",
            borderColor: "divider",
            backgroundImage: "none",
            display: "flex",
            flexDirection: "column",
          },
        },
      }}
    >
      {/* Header */}
      <Box sx={{ p: "24px 24px 0" }}>
        <Typography
          component="h2"
          sx={{ fontSize: 22, fontWeight: 500, letterSpacing: "-0.01em" }}
        >
          Create a decentralized party
        </Typography>
        <Typography
          sx={{ fontSize: 13, lineHeight: 1.5, color: "text.secondary", mt: 1 }}
        >
          Starts an onboarding workflow that coordinates with the selected peers
          to establish the party topology and namespace.
        </Typography>
      </Box>

      {/* Body — fills the modal; only the peer list scrolls. */}
      <Box
        sx={{
          p: "20px 24px",
          flex: 1,
          minHeight: 0,
          overflow: "hidden",
          display: "flex",
          flexDirection: "column",
          gap: 2.25,
        }}
      >
        <Box sx={{ flexShrink: 0 }}>
          <TextField
            label="Party ID prefix"
            value={partyIdPrefix}
            onChange={(e) => setPartyIdPrefix(e.target.value)}
            placeholder="my-network"
            fullWidth
            disabled={controlsDisabled}
            error={!!partyPrefixError(partyIdPrefix.trim())}
            helperText={
              partyPrefixError(partyIdPrefix.trim()) ??
              "Letters, digits, “-” and “_”. Must start with a letter."
            }
            sx={{ "& input": { fontFamily: "var(--font-mono)" } }}
            slotProps={{
              input: {
                endAdornment: fieldHelpAdornment(
                  "The human-readable name for the new decentralized party. It shows up as the bit before '::' in the party id and must be unique on this node. Cannot be changed after creation.",
                  "Help for Party ID Prefix",
                ),
              },
            }}
          />
          {prefixValid && (
            <Typography
              sx={{
                fontFamily: "var(--font-mono)",
                fontSize: 12,
                color: "text.secondary",
                mt: 1,
              }}
            >
              Party id&nbsp;→&nbsp;
              <Box component="span" sx={{ color: "text.primary" }}>
                {partyIdPrefix.trim()}
              </Box>
              ::
              <Box component="span" sx={{ color: "text.disabled" }}>
                &lt;namespace&gt;
              </Box>
            </Typography>
          )}
        </Box>

        <Box
          sx={{ height: "1px", bgcolor: "divider", mx: "-24px", flexShrink: 0 }}
        />

        {/* Peers to invite — this section flexes to fill, the list scrolls. */}
        <Box sx={{ flex: 1, minHeight: 0, display: "flex", flexDirection: "column" }}>
          <Box
            sx={{
              flexShrink: 0,
              display: "flex",
              alignItems: "center",
              justifyContent: "space-between",
              gap: 1.5,
            }}
          >
            <Typography
              sx={{
                fontFamily: "var(--font-mono)",
                fontSize: 11,
                fontWeight: 500,
                letterSpacing: "0.1em",
                textTransform: "uppercase",
                color: "text.secondary",
              }}
            >
              Peers to invite
            </Typography>
            {selectablePeers.length > 0 && (
              <Box sx={{ display: "flex", alignItems: "center", gap: 1.5 }}>
                <Typography
                  sx={{
                    fontFamily: "var(--font-mono)",
                    fontSize: 12,
                    color: "text.secondary",
                  }}
                >
                  <Box
                    component="span"
                    sx={{ color: "var(--accent)", fontWeight: 500 }}
                  >
                    {selectedPeerIds.size}
                  </Box>{" "}
                  of {selectablePeers.length}
                </Typography>
                <Button
                  variant="text"
                  size="small"
                  onClick={toggleAllVisible}
                  disabled={controlsDisabled || visiblePeers.length === 0}
                  sx={{ minWidth: 0, p: 0, fontWeight: 500 }}
                >
                  {allVisibleSelected ? "Clear" : "Select all"}
                </Button>
              </Box>
            )}
          </Box>

          {loadingPeers ? (
            <Box sx={{ display: "flex", justifyContent: "center", py: 3 }}>
              <CircularProgress size={22} />
            </Box>
          ) : selectablePeers.length === 0 ? (
            <Typography
              sx={{ fontSize: 13, color: "text.secondary", mt: 1.5 }}
            >
              No peers configured. Add peers in Network Configuration first.
            </Typography>
          ) : (
            <>
              {showFilter && (
                <TextField
                  size="small"
                  fullWidth
                  value={filter}
                  onChange={(e) => setFilter(e.target.value)}
                  placeholder="Filter by name or address"
                  disabled={controlsDisabled}
                  sx={{ mt: 1.5, flexShrink: 0 }}
                  slotProps={{
                    input: {
                      startAdornment: (
                        <InputAdornment position="start">
                          <SearchIcon fontSize="small" />
                        </InputAdornment>
                      ),
                    },
                  }}
                />
              )}
              <Box
                sx={{
                  mt: 1.25,
                  flex: 1,
                  minHeight: 80,
                  border: "1px solid",
                  borderColor: "divider",
                  borderRadius: "8px",
                  bgcolor: "background.default",
                  overflowY: "auto",
                  p: 0.5,
                  display: "flex",
                  flexDirection: "column",
                  gap: "4px",
                }}
              >
                {visiblePeers.length === 0 ? (
                  <Typography
                    sx={{
                      fontSize: 13,
                      color: "text.disabled",
                      textAlign: "center",
                      py: 3,
                    }}
                  >
                    No peers match “{filter.trim()}”.
                  </Typography>
                ) : (
                  visiblePeers.map((peer) => {
                    const sel = selectedPeerIds.has(peer.participant_id);
                    return (
                      <Box
                        key={peer.participant_id}
                        role="checkbox"
                        aria-checked={sel}
                        tabIndex={controlsDisabled ? -1 : 0}
                        onClick={() =>
                          !controlsDisabled && togglePeer(peer.participant_id)
                        }
                        onKeyDown={(e) => {
                          if (
                            (e.key === "Enter" || e.key === " ") &&
                            !controlsDisabled
                          ) {
                            e.preventDefault();
                            togglePeer(peer.participant_id);
                          }
                        }}
                        sx={{
                          display: "flex",
                          alignItems: "center",
                          gap: 1.5,
                          p: "9px 10px",
                          borderRadius: "6px",
                          cursor: controlsDisabled ? "default" : "pointer",
                          opacity: controlsDisabled ? 0.6 : 1,
                          bgcolor: sel
                            ? "rgba(214, 58, 15, 0.08)"
                            : "transparent",
                          transition: "background-color 0.12s ease-out",
                          "&:hover": controlsDisabled
                            ? undefined
                            : {
                                bgcolor: sel
                                  ? "rgba(214, 58, 15, 0.14)"
                                  : "action.hover",
                              },
                        }}
                      >
                        <Box
                          sx={{
                            flexShrink: 0,
                            width: 18,
                            height: 18,
                            borderRadius: "4px",
                            border: "1.5px solid",
                            borderColor: sel ? "var(--accent)" : "text.secondary",
                            bgcolor: sel ? "var(--accent)" : "transparent",
                            color: "#fff",
                            display: "grid",
                            placeItems: "center",
                            transition: "0.12s ease-out",
                          }}
                        >
                          {sel && <CheckIcon sx={{ fontSize: 13 }} />}
                        </Box>
                        <Box sx={{ flex: 1, minWidth: 0 }}>
                          <Typography
                            sx={{ fontSize: 14, fontWeight: 500, lineHeight: 1.3 }}
                          >
                            {peer.name || peer.participant_id}
                          </Typography>
                          <Typography
                            sx={{
                              fontFamily: "var(--font-mono)",
                              fontSize: 12,
                              color: "text.secondary",
                              whiteSpace: "nowrap",
                              overflow: "hidden",
                              textOverflow: "ellipsis",
                            }}
                          >
                            {peer.address}:{peer.port}
                          </Typography>
                        </Box>
                      </Box>
                    );
                  })
                )}
              </Box>
            </>
          )}
        </Box>

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

      {/* Footer */}
      <Box
        sx={{
          display: "flex",
          alignItems: "center",
          justifyContent: "flex-end",
          gap: 1.25,
          p: "16px 24px",
          borderTop: "1px solid",
          borderColor: "divider",
        }}
      >
        {isStartState && selectablePeers.length > 0 && (
          <Typography
            sx={{
              mr: "auto",
              fontFamily: "var(--font-mono)",
              fontSize: 12,
              color: "text.disabled",
            }}
          >
            {footerNote}
          </Typography>
        )}
        <Button
          onClick={handleClose}
          disabled={loading}
          sx={{ color: "text.secondary" }}
        >
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
            {cancelling ? "Cancelling…" : "Cancel workflow"}
          </Button>
        )}
        {isStartState ? (
          <Button
            onClick={handleStart}
            variant="contained"
            color="primary"
            disabled={
              loading ||
              !partyIdPrefix.trim() ||
              !!partyPrefixError(partyIdPrefix.trim()) ||
              selectedPeerIds.size === 0
            }
          >
            {loading ? <CircularProgress size={20} /> : "Start onboarding"}
          </Button>
        ) : null}
      </Box>
    </Dialog>
  );
};
