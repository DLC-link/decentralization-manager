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
  Stack,
  Tooltip,
  useMediaQuery,
  useTheme,
} from "@mui/material";
import CircleIcon from "@mui/icons-material/Circle";
import EditIcon from "@mui/icons-material/Edit";
import DeleteIcon from "@mui/icons-material/Delete";
import AddIcon from "@mui/icons-material/Add";
import SaveIcon from "@mui/icons-material/Save";
import CancelIcon from "@mui/icons-material/Cancel";
import PersonIcon from "@mui/icons-material/Person";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import ContentPasteIcon from "@mui/icons-material/ContentPaste";
import { useSnackbar } from "../contexts";
import { zebraRow } from "../styles";
import { copyToClipboard } from "../clipboard";
import { fieldHelpAdornment } from "./FieldHelp";
import type {
  NetworkConfig,
  Peer,
  ParticipantStatus,
  NodeConfig,
  KeyStatusResponse,
  ConnectionStatus,
} from "../types";

interface NetworkConfigAccordionProps {
  config: NetworkConfig;
  nodeConfig?: NodeConfig;
  keyStatus?: KeyStatusResponse;
  participantStatuses?: ParticipantStatus[];
  onSave?: (peers: Peer[]) => Promise<void>;
}

const emptyPeer: Peer = {
  participant_id: "",
  name: "",
  address: "localhost",
  port: 9000,
  public_key: "",
};

export const NetworkConfigAccordion = ({
  config,
  nodeConfig,
  keyStatus,
  participantStatuses,
  onSave,
}: NetworkConfigAccordionProps) => {
  const [editing, setEditing] = useState(false);
  const [editedPeers, setEditedPeers] = useState<Peer[]>([]);
  const [saving, setSaving] = useState(false);
  const { showSnackbar } = useSnackbar();
  const theme = useTheme();
  const isSmall = useMediaQuery(theme.breakpoints.down("sm"));
  const isMedium = useMediaQuery(theme.breakpoints.down("md"));

  const selfNodeId = nodeConfig?.node.participant_id;
  const selfPublicKey = keyStatus?.public_key || "";
  const selfPort = nodeConfig?.node.port ?? 9000;

  const truncateKey = (key: string): string => {
    if (!key) return "-";
    const len = isSmall ? 6 : isMedium ? 8 : 12;
    return `${key.slice(0, len)}...${key.slice(-4)}`;
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

  const getStatus = (id: string): ConnectionStatus | undefined =>
    participantStatuses?.find((s) => s.id === id)?.status;

  const getStatusColor = (status: ConnectionStatus | undefined): string => {
    switch (status) {
      case "Connected":
        return "success.main";
      case "CurrentNode":
        return "primary.main";
      case "Unreachable":
      case "HandshakeFailed":
        return "error.main";
      default:
        return "text.disabled";
    }
  };

  const getStatusTooltip = (status: ConnectionStatus | undefined): string => {
    switch (status) {
      case "Connected":
        return "Connected via Noise protocol";
      case "CurrentNode":
        return "This is the current node";
      case "Unreachable":
        return "Cannot reach peer (TCP connection failed)";
      case "HandshakeFailed":
        return "Noise handshake failed - check if the public key is correct";
      default:
        return "Status unknown";
    }
  };

  // Build display list: self first, then other peers
  const selfPeer = config.peers.find((p) => p.participant_id === selfNodeId);
  const otherPeers = config.peers.filter((p) => p.participant_id !== selfNodeId);

  // Create self entry if not in peers list
  const selfEntry: Peer | null = selfNodeId
    ? selfPeer || {
        participant_id: selfNodeId,
        name: selfNodeId,
        address: nodeConfig?.node.public_address || nodeConfig?.node.listen_address || "localhost",
        port: selfPort,
        public_key: selfPublicKey,
      }
    : null;

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
    try {
      const text = await navigator.clipboard.readText();
      const parts = text.trim().split(",");
      if (parts.length < 5) {
        showSnackbar(
          "Invalid CSV format. Expected: participant_id,name,address,port,public_key",
          "error",
        );
        return;
      }
      const [participant_id, name, address, portStr, public_key] = parts;
      const port = parseInt(portStr) || 9000;
      const newPeer: Peer = { participant_id, name, address, port, public_key };
      setEditedPeers((peers) => [...peers, newPeer]);
      showSnackbar("Peer added from clipboard");
    } catch {
      showSnackbar("Failed to read clipboard", "error");
    }
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
                  gridTemplateColumns: "1fr 1fr 1fr 100px 1fr auto",
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
                  label="Address"
                  value={peer.address}
                  onChange={(e) => updatePeer(index, "address", e.target.value)}
                  slotProps={{
                    input: {
                      endAdornment: fieldHelpAdornment(
                        "The hostname or IP address where your local node's Noise client will connect to this peer.",
                        "Help for Address",
                      ),
                    },
                  }}
                />
                <TextField
                  size="small"
                  label="Port"
                  type="number"
                  value={peer.port}
                  onChange={(e) =>
                    updatePeer(index, "port", parseInt(e.target.value) || 0)
                  }
                  slotProps={{
                    input: {
                      endAdornment: fieldHelpAdornment(
                        "The TCP port the peer's Noise server is listening on. Combined with Address to dial the peer.",
                        "Help for Port",
                      ),
                    },
                  }}
                />
                <TextField
                  size="small"
                  label="Public Key"
                  value={peer.public_key}
                  onChange={(e) =>
                    updatePeer(index, "public_key", e.target.value)
                  }
                  slotProps={{
                    input: {
                      endAdornment: fieldHelpAdornment(
                        "The peer's Noise public key (hex-encoded). Used to derive the pre-shared key that secures the encrypted channel.",
                        "Help for Public Key",
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
      <Box sx={{ display: "flex", justifyContent: "space-between", alignItems: "center", px: 3, py: 2 }}>
            <Typography variant="subtitle1">Peers:</Typography>
            <Box sx={{ display: "flex", gap: 1 }}>
              {selfEntry && (
                <Button
                  size="small"
                  variant="outlined"
                  startIcon={<ContentCopyIcon />}
                  onClick={async () => {
                    const name = selfPeer?.name || truncateParticipantId(selfEntry.participant_id);
                    const csvRow = `${selfEntry.participant_id},${name},${selfEntry.address},${selfEntry.port},${selfEntry.public_key},`;
                    const success = await copyToClipboard(csvRow);
                    showSnackbar(success ? "Copied to clipboard" : "Failed to copy");
                  }}
                >
                  Share my data
                </Button>
              )}
              {onSave && (
                <Tooltip title="Edit peers">
                  <IconButton size="small" onClick={startEditing}>
                    <EditIcon fontSize="small" />
                  </IconButton>
                </Tooltip>
              )}
            </Box>
          </Box>
          <Box sx={{ overflowX: "auto" }}>
            <Table size="small" sx={{ minWidth: 650 }}>
              <TableHead>
                <TableRow>
                  <TableCell sx={{ py: 1, width: 50 }}>Status</TableCell>
                  <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>Name</TableCell>
                  <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>Address</TableCell>
                  <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>Public Key</TableCell>
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
                    <Typography variant="body2" color="text.secondary">
                      {selfPeer?.name || truncateParticipantId(selfEntry.participant_id)} (You)
                    </Typography>
                  </TableCell>
                  <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>
                    {selfEntry.address}:{selfEntry.port}
                  </TableCell>
                  <TableCell
                    sx={{ fontFamily: "monospace", fontSize: "0.75rem", py: 1 }}
                  >
                    {truncateKey(selfEntry.public_key)}
                  </TableCell>
                </TableRow>
              )}
              {otherPeers.map((p, idx) => {
                const status = getStatus(p.participant_id);
                return (
                  <TableRow key={p.participant_id} sx={zebraRow(idx)}>
                    <TableCell sx={{ py: 1 }}>
                      <Tooltip title={getStatusTooltip(status)} arrow>
                        <CircleIcon
                          sx={{
                            fontSize: 12,
                            color: getStatusColor(status),
                            cursor: "help",
                          }}
                        />
                      </Tooltip>
                    </TableCell>
                    <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>
                      {p.name || truncateParticipantId(p.participant_id)}
                    </TableCell>
                    <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>
                      {p.address}:{p.port}
                    </TableCell>
                    <TableCell
                      sx={{ fontFamily: "monospace", fontSize: "0.75rem", py: 1 }}
                    >
                      {truncateKey(p.public_key)}
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
