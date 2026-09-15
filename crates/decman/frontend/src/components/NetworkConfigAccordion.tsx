import { useState } from "react";
import {
  Typography,
  Box,
  Table,
  TableHead,
  TableBody,
  TableRow,
  TableCell,
  IconButton,
  TextField,
  Button,
  Chip,
  Stack,
  Tooltip,
  useMediaQuery,
  useTheme,
} from "@mui/material";
import EditIcon from "@mui/icons-material/Edit";
import DeleteIcon from "@mui/icons-material/Delete";
import AddIcon from "@mui/icons-material/Add";
import SaveIcon from "@mui/icons-material/Save";
import CancelIcon from "@mui/icons-material/Cancel";
import PersonIcon from "@mui/icons-material/Person";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import ContentPasteIcon from "@mui/icons-material/ContentPaste";
import DownloadIcon from "@mui/icons-material/Download";
import UploadFileIcon from "@mui/icons-material/UploadFile";
import { ADMIN_ACCESS } from "../constants";
import { useSnackbar } from "../contexts";
import { zebraRow } from "../styles";
import { copyToClipboard } from "../clipboard";
import { fieldHelpAdornment } from "./FieldHelp";
import { StatusDot } from "./StatusDot";
import { PeersCsvDialog, type PeersCsvMode } from "./PeersCsvDialog";
import { parsePeersCsv, peerToCsvRow } from "../peerCsv";
import { NO_NODE_PARTY, heartbeatAge, toneForPeer } from "../peers";
import type {
  NetworkConfig,
  Peer,
  ParticipantStatus,
  NodeConfig,
  NodeIdentityResponse,
  ConnectionStatus,
} from "../types";

interface NetworkConfigAccordionProps {
  config: NetworkConfig;
  nodeConfig?: NodeConfig;
  /** This node's own identity, for the "you" row and the share button. */
  nodeIdentity?: NodeIdentityResponse;
  participantStatuses?: ParticipantStatus[];
  /** Our own round-trip latency to the backend (ms), shown on the "you" row. */
  selfLatencyMs?: number;
  onSave?: (peers: Peer[]) => Promise<void>;
}

const emptyPeer: Peer = {
  participant_id: "",
  name: "",
  party: "",
};

export const NetworkConfigAccordion = ({
  config,
  nodeConfig,
  nodeIdentity,
  participantStatuses,
  selfLatencyMs,
  onSave,
}: NetworkConfigAccordionProps) => {
  const [editing, setEditing] = useState(false);
  const [editedPeers, setEditedPeers] = useState<Peer[]>([]);
  const [saving, setSaving] = useState(false);
  const [csvMode, setCsvMode] = useState<PeersCsvMode | null>(null);
  const { showSnackbar } = useSnackbar();
  const theme = useTheme();
  const isSmall = useMediaQuery(theme.breakpoints.down("sm"));
  const isMedium = useMediaQuery(theme.breakpoints.down("md"));

  const selfNodeId = nodeConfig?.node.participant_id;
  const selfNodeParty = nodeIdentity?.node_party_id ?? "";

  const truncateParty = (party?: string): string => {
    if (!party) return NO_NODE_PARTY;
    const len = isSmall ? 8 : isMedium ? 14 : 24;
    return party.length > len + 6 ? `${party.slice(0, len)}...${party.slice(-6)}` : party;
  };

  // Truncate participant ID: prefix::1220...last4
  const truncateParticipantId = (id: string): string => {
    if (!id) return "";
    const parts = id.split("::");
    if (parts.length !== 2) return id;
    const [prefix, namespace] = parts;
    if (namespace.length <= 8) return id;
    return `${prefix}::${namespace.slice(0, 4)}...${namespace.slice(-4)}`;
  };

  const getStat = (id: string): ParticipantStatus | undefined =>
    participantStatuses?.find((s) => s.id === id);

  // A peer's status is the age of its registry heartbeat, not a connection:
  // nodes never dial each other (design D3).
  const getStatusTooltip = (status: ConnectionStatus | undefined): string => {
    switch (status) {
      case "CurrentNode":
        return "This is the current node";
      case "Active":
        return "The peer published a recent heartbeat";
      case "Stale":
        return "No recent heartbeat. The peer may be down, or it may have stopped publishing";
      case "Unvetted":
        return "The peer's participant has not vetted the coordination package, so it cannot be invited";
      default:
        return "No registry entry visible. The peer may not have added you yet";
    }
  };

  // Tooltip enriched with the heartbeat age.
  const statusTooltip = (st: ParticipantStatus | undefined): string => {
    const title = getStatusTooltip(st?.status);
    return st?.heartbeat_age_secs == null
      ? title
      : `${title} — last heartbeat ${heartbeatAge(st.heartbeat_age_secs)}`;
  };

  // Build display list: self first, then other peers
  const selfPeer = config.peers.find((p) => p.participant_id === selfNodeId);
  const otherPeers = config.peers.filter((p) => p.participant_id !== selfNodeId);

  // Create self entry if not in peers list
  const selfEntry: Peer | null = selfNodeId
    ? (selfPeer ?? {
        participant_id: selfNodeId,
        name: selfNodeId,
        party: selfNodeParty,
      })
    : null;

  const exportablePeers: Peer[] =
    selfEntry && !selfPeer ? [selfEntry, ...config.peers] : config.peers;

  const startEditing = () => {
    setEditedPeers(config.peers.map((p) => ({ ...p })));
    setEditing(true);
  };

  const cancelEditing = () => {
    setEditing(false);
    setEditedPeers([]);
  };

  const handleSave = async () => {
    if (!onSave) return;
    setSaving(true);
    try {
      await onSave(editedPeers);
      setEditing(false);
    } catch (e) {
      console.error("Failed to save peers:", e);
    } finally {
      setSaving(false);
    }
  };

  const updatePeer = (
    index: number,
    field: keyof Peer,
    value: string | number,
  ) => {
    setEditedPeers((peers) =>
      peers.map((p, i) => (i === index ? { ...p, [field]: value } : p)),
    );
  };

  const addPeer = () => {
    setEditedPeers((peers) => [...peers, { ...emptyPeer }]);
  };

  const addPeerFromClipboard = async () => {
    let text: string;
    try {
      text = await navigator.clipboard.readText();
    } catch {
      showSnackbar("Failed to read clipboard", "error");
      return;
    }
    const { rows, rejected } = parsePeersCsv(text);
    const skipped = rejected
      .map((r) => `line ${r.line}: ${r.reason}`)
      .join("; ");
    if (rows.length === 0) {
      showSnackbar(
        skipped || "Expected: participant_id,node_party_id,name",
        "error",
      );
      return;
    }
    setEditedPeers((peers) => [...peers, ...rows.map((r) => r.peer)]);
    const added =
      rows.length === 1
        ? "Peer added from clipboard"
        : `${rows.length} peers added from clipboard`;
    showSnackbar(
      skipped ? `${added}. Skipped ${skipped}` : added,
      skipped ? "error" : "info",
    );
  };

  const removePeer = (index: number) => {
    setEditedPeers((peers) => peers.filter((_, i) => i !== index));
  };

  if (editing) {
    return (
      <Box sx={{ p: 2 }}>
        <Typography variant="subtitle2" color="text.secondary" sx={{ mb: 2 }}>
          Edit Peers
        </Typography>
          <Stack spacing={2}>
            {editedPeers.map((peer, index) => (
              <Box
                key={index}
                sx={{
                  display: "grid",
                  gridTemplateColumns: "1fr 1fr 1.5fr auto",
                  gap: 1,
                  alignItems: "center",
                }}
              >
                <TextField
                  size="small"
                  label="Participant ID"
                  value={peer.participant_id}
                  onChange={(e) => updatePeer(index, "participant_id", e.target.value)}
                  slotProps={{
                    input: {
                      endAdornment: fieldHelpAdornment(
                        "The Canton ID of the remote node, like \"validator-1::1220abc...\". Used as the unique key for this peer on your local peers table.",
                        "Help for Participant ID",
                      ),
                    },
                  }}
                />
                <TextField
                  size="small"
                  label="Name"
                  value={peer.name}
                  onChange={(e) => updatePeer(index, "name", e.target.value)}
                  slotProps={{
                    input: {
                      endAdornment: fieldHelpAdornment(
                        "A human-readable label for this peer that shows up in the UI. Any text you like.",
                        "Help for Name",
                      ),
                    },
                  }}
                />
                <TextField
                  size="small"
                  label="Node Party"
                  value={peer.party ?? ""}
                  error={!peer.party?.trim()}
                  onChange={(e) => updatePeer(index, "party", e.target.value)}
                  slotProps={{
                    input: {
                      endAdornment: fieldHelpAdornment(
                        "The peer's node party, like \"node1::1220abc...\". Your node names it as an observer of every contract it writes for this peer, so a peer without one cannot be invited to a workflow.",
                        "Help for Node Party",
                      ),
                    },
                  }}
                />
                <Tooltip title="Remove peer">
                  <IconButton
                    color="error"
                    onClick={() => removePeer(index)}
                    size="small"
                  >
                    <DeleteIcon />
                  </IconButton>
                </Tooltip>
              </Box>
            ))}
            <Box
              sx={{ display: "flex", gap: 1, justifyContent: "space-between" }}
            >
              <Box sx={{ display: "flex", gap: 1 }}>
                <Button
                  startIcon={<AddIcon />}
                  onClick={addPeer}
                  variant="outlined"
                  size="small"
                >
                  Add Peer
                </Button>
                <Button
                  startIcon={<ContentPasteIcon />}
                  onClick={addPeerFromClipboard}
                  variant="outlined"
                  size="small"
                >
                  Paste from Clipboard
                </Button>
              </Box>
              <Box sx={{ display: "flex", gap: 1 }}>
                <Button
                  startIcon={<CancelIcon />}
                  onClick={cancelEditing}
                  variant="outlined"
                  size="small"
                  disabled={saving}
                >
                  Cancel
                </Button>
                <Button
                  startIcon={<SaveIcon />}
                  onClick={handleSave}
                  variant="contained"
                  size="small"
                  disabled={saving}
                >
                  {saving ? "Saving..." : "Save"}
                </Button>
              </Box>
            </Box>
          </Stack>
      </Box>
    );
  }

  return (
    <Box>
      <Box sx={{ display: "flex", justifyContent: "space-between", alignItems: "center", px: "var(--content-pad)", py: 2 }}>
            <Typography variant="subtitle1">Peers:</Typography>
            <Box sx={{ display: "flex", gap: 1 }}>
              {selfEntry && (
                <Tooltip
                  title={
                    selfEntry.party
                      ? "Copy participant_id,node_party_id,name for a peer to paste"
                      : "Set this node's identity first: a peer needs your node party to invite you"
                  }
                >
                  {/* The span keeps the tooltip alive over a disabled button. */}
                  <span>
                    <Button
                      size="small"
                      variant="outlined"
                      startIcon={<ContentCopyIcon />}
                      disabled={!selfEntry.party}
                      onClick={async () => {
                        const name =
                          selfPeer?.name || truncateParticipantId(selfEntry.participant_id);
                        const success = await copyToClipboard(
                          peerToCsvRow({ ...selfEntry, name }),
                        );
                        showSnackbar(success ? "Copied to clipboard" : "Failed to copy");
                      }}
                    >
                      Share my identity
                    </Button>
                  </span>
                </Tooltip>
              )}
              <Button
                size="small"
                variant="outlined"
                startIcon={<DownloadIcon />}
                onClick={() => setCsvMode("export")}
              >
                Export CSV
              </Button>
              {onSave && (
                <>
                  {ADMIN_ACCESS && (
                    <Button
                      size="small"
                      variant="outlined"
                      startIcon={<UploadFileIcon />}
                      onClick={() => setCsvMode("import")}
                    >
                      Import CSV
                    </Button>
                  )}
                  <Tooltip title="Edit peers">
                    <IconButton size="small" onClick={startEditing}>
                      <EditIcon fontSize="small" />
                    </IconButton>
                  </Tooltip>
                </>
              )}
            </Box>
          </Box>
          <PeersCsvDialog
            open={csvMode !== null}
            mode={csvMode ?? "export"}
            peers={csvMode === "import" ? config.peers : exportablePeers}
            selfNodeId={selfNodeId}
            onClose={() => setCsvMode(null)}
            onSave={ADMIN_ACCESS ? onSave : undefined}
          />
          <Box sx={{ overflowX: "auto" }}>
            <Table size="small" sx={{ minWidth: 650 }}>
              <TableHead>
                <TableRow>
                  <TableCell sx={{ py: 1, width: 50 }}>Status</TableCell>
                  <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>Name</TableCell>
                  <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>Node Party</TableCell>
                  <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>Heartbeat</TableCell>
                  <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>Version</TableCell>
                </TableRow>
              </TableHead>
            <TableBody>
              {selfEntry && (
                <TableRow sx={{ bgcolor: "action.selected" }}>
                  <TableCell sx={{ py: 1 }}>
                    <Tooltip title="This is your node" arrow>
                      <PersonIcon sx={{ fontSize: 14, color: "primary.main" }} />
                    </Tooltip>
                  </TableCell>
                  <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>
                    <Typography
                      variant="body2"
                      color="text.secondary"
                      component="span"
                    >
                      {selfPeer?.name || truncateParticipantId(selfEntry.participant_id)} (You)
                    </Typography>
                    {selfLatencyMs != null && (
                      <Tooltip title="Round-trip from this browser to your node" arrow>
                        <Typography
                          component="span"
                          tabIndex={0}
                          sx={{
                            ml: 1,
                            color: "text.secondary",
                            fontSize: "0.7rem",
                            cursor: "help",
                          }}
                        >
                          {selfLatencyMs} ms
                        </Typography>
                      </Tooltip>
                    )}
                  </TableCell>
                  <TableCell
                    sx={{
                      fontFamily: "var(--font-mono)",
                      fontSize: "0.75rem",
                      py: 1,
                      whiteSpace: "nowrap",
                      color: selfEntry.party ? undefined : "text.disabled",
                    }}
                  >
                    {truncateParty(selfEntry.party)}
                  </TableCell>
                  {/* This node publishes its own heartbeat; it never reads one
                    * back for itself. */}
                  <TableCell sx={{ py: 1, whiteSpace: "nowrap", color: "text.disabled" }}>
                    —
                  </TableCell>
                  <TableCell
                    sx={{ fontFamily: "var(--font-mono)", fontSize: "0.75rem", py: 1, whiteSpace: "nowrap" }}
                  >
                    {getStat(selfEntry.participant_id)?.build_version ??
                      getStat(selfEntry.participant_id)?.version ??
                      nodeConfig?.build_version ??
                      nodeConfig?.version ??
                      "—"}
                  </TableCell>
                </TableRow>
              )}
              {otherPeers.map((p, idx) => {
                const st = getStat(p.participant_id);
                return (
                  <TableRow key={p.participant_id} sx={zebraRow(idx)}>
                    <TableCell sx={{ py: 1 }}>
                      <StatusDot tone={toneForPeer(st?.status)} title={statusTooltip(st)} />
                    </TableCell>
                    <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>
                      {p.name || truncateParticipantId(p.participant_id)}
                      {!p.party && (
                        <Chip
                          size="small"
                          color="warning"
                          label="No node party"
                          sx={{ ml: 1, height: 18, fontSize: "0.65rem" }}
                        />
                      )}
                    </TableCell>
                    <TableCell
                      sx={{
                        fontFamily: "var(--font-mono)",
                        fontSize: "0.75rem",
                        py: 1,
                        whiteSpace: "nowrap",
                        color: p.party ? undefined : "text.disabled",
                      }}
                    >
                      {truncateParty(p.party)}
                    </TableCell>
                    <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>
                      {heartbeatAge(st?.heartbeat_age_secs)}
                    </TableCell>
                    <TableCell
                      sx={{ fontFamily: "var(--font-mono)", fontSize: "0.75rem", py: 1, whiteSpace: "nowrap" }}
                    >
                      {/* Falls back to the compatibility semver: a peer that
                        * predates `build_version` still reports `version`. */}
                      {st?.build_version ?? st?.version ?? "—"}
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </Box>
    </Box>
  );
};
