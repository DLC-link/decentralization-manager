import { useState } from "react";
import {
  Accordion,
  AccordionSummary,
  AccordionDetails,
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
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
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
import { copyToClipboard } from "./CopyableText";
import type {
  NetworkConfig,
  Peer,
  ParticipantStatus,
  NodeConfig,
  KeyStatusResponse,
  ConnectionStatus,
} from "../types";

const accordionSx = {
  borderRadius: 2,
  mb: 2,
  "&:first-of-type": { borderRadius: 2 },
  "&:last-of-type": { borderRadius: 2 },
  overflow: "hidden",
};

interface NetworkConfigAccordionProps {
  config: NetworkConfig;
  nodeConfig?: NodeConfig;
  keyStatus?: KeyStatusResponse;
  participantStatuses?: ParticipantStatus[];
  onSave?: (peers: Peer[]) => Promise<void>;
}

const emptyPeer: Peer = {
  id: "",
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

  const selfNodeId = nodeConfig?.node.node_id;
  const selfPublicKey = keyStatus?.public_key || "";
  const selfPort = nodeConfig?.node.port ?? 9000;

  const truncateKey = (key: string): string => {
    if (!key) return "-";
    const len = isSmall ? 8 : isMedium ? 12 : 16;
    return `${key.slice(0, len)}...`;
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
  const selfPeer = config.peers.find((p) => p.id === selfNodeId);
  const otherPeers = config.peers.filter((p) => p.id !== selfNodeId);

  // Create self entry if not in peers list
  const selfEntry: Peer | null = selfNodeId
    ? selfPeer || {
        id: selfNodeId,
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
        showSnackbar("Invalid CSV format. Expected: id,name,address,port,public_key");
        return;
      }
      const [id, name, address, portStr, public_key] = parts;
      const port = parseInt(portStr) || 9000;
      const newPeer: Peer = { id, name, address, port, public_key };
      setEditedPeers((peers) => [...peers, newPeer]);
      showSnackbar("Peer added from clipboard");
    } catch {
      showSnackbar("Failed to read clipboard");
    }
  };

  const removePeer = (index: number) => {
    setEditedPeers((peers) => peers.filter((_, i) => i !== index));
  };

  if (editing) {
    return (
      <Accordion sx={accordionSx} defaultExpanded>
        <AccordionSummary
          expandIcon={<ExpandMoreIcon />}
          sx={{ borderRadius: "8px 8px 0 0" }}
        >
          <Typography variant="h6">Edit Peers</Typography>
        </AccordionSummary>
        <AccordionDetails sx={{ p: 3 }}>
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
                  label="ID"
                  value={peer.id}
                  onChange={(e) => updatePeer(index, "id", e.target.value)}
                />
                <TextField
                  size="small"
                  label="Name"
                  value={peer.name}
                  onChange={(e) => updatePeer(index, "name", e.target.value)}
                />
                <TextField
                  size="small"
                  label="Address"
                  value={peer.address}
                  onChange={(e) => updatePeer(index, "address", e.target.value)}
                />
                <TextField
                  size="small"
                  label="Port"
                  type="number"
                  value={peer.port}
                  onChange={(e) =>
                    updatePeer(index, "port", parseInt(e.target.value) || 0)
                  }
                />
                <TextField
                  size="small"
                  label="Public Key"
                  value={peer.public_key}
                  onChange={(e) =>
                    updatePeer(index, "public_key", e.target.value)
                  }
                />
                <IconButton
                  color="error"
                  onClick={() => removePeer(index)}
                  size="small"
                >
                  <DeleteIcon />
                </IconButton>
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
        </AccordionDetails>
      </Accordion>
    );
  }

  return (
    <Accordion sx={accordionSx}>
      <AccordionSummary
        expandIcon={<ExpandMoreIcon />}
        sx={{ borderRadius: "8px 8px 0 0" }}
      >
        <Typography variant="h6">Network Configuration</Typography>
      </AccordionSummary>
      <AccordionDetails sx={{ p: 0 }}>
        <Box>
          <Box sx={{ display: "flex", justifyContent: "space-between", px: 2, py: 1 }}>
            <Typography variant="subtitle1">Peers:</Typography>
            {onSave && (
              <IconButton size="small" onClick={startEditing}>
                <EditIcon fontSize="small" />
              </IconButton>
            )}
          </Box>
          <Box sx={{ overflowX: "auto" }}>
            <Table size="small">
              <TableHead>
                <TableRow>
                  <TableCell sx={{ py: 1 }}>Status</TableCell>
                  <TableCell sx={{ py: 1 }}>ID</TableCell>
                  <TableCell sx={{ py: 1 }}>Name</TableCell>
                  <TableCell sx={{ py: 1 }}>Address</TableCell>
                  <TableCell sx={{ py: 1 }}>Public Key</TableCell>
                  <TableCell sx={{ py: 1, width: 40 }}></TableCell>
                </TableRow>
              </TableHead>
            <TableBody>
              {selfEntry && (
                <TableRow sx={{ bgcolor: "action.selected" }}>
                  <TableCell sx={{ py: 1 }}>
                    <PersonIcon sx={{ fontSize: 14, color: "primary.main" }} />
                  </TableCell>
                  <TableCell sx={{ py: 1 }}>
                    <Box
                      sx={{ display: "flex", alignItems: "center", gap: 0.5 }}
                    >
                      {selfEntry.id}
                      <Typography variant="caption" color="text.secondary">
                        (you)
                      </Typography>
                    </Box>
                  </TableCell>
                  <TableCell sx={{ py: 1 }}>{selfEntry.name}</TableCell>
                  <TableCell sx={{ py: 1 }}>
                    {selfEntry.address}:{selfEntry.port}
                  </TableCell>
                  <TableCell
                    sx={{ fontFamily: "monospace", fontSize: "0.75rem", py: 1 }}
                  >
                    {truncateKey(selfEntry.public_key)}
                  </TableCell>
                  <TableCell sx={{ py: 1 }}>
                    <Tooltip title="Copy as CSV row">
                      <IconButton
                        size="small"
                        onClick={async () => {
                          const name = selfPeer?.name || selfEntry.id;
                          const csvRow = `${selfEntry.id},${name},${selfEntry.address},${selfEntry.port},${selfEntry.public_key},`;
                          const success = await copyToClipboard(csvRow);
                          showSnackbar(success ? "Copied to clipboard" : "Failed to copy");
                        }}
                      >
                        <ContentCopyIcon fontSize="small" />
                      </IconButton>
                    </Tooltip>
                  </TableCell>
                </TableRow>
              )}
              {otherPeers.map((p) => {
                const status = getStatus(p.id);
                return (
                  <TableRow key={p.id}>
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
                    <TableCell sx={{ py: 1 }}>{p.id}</TableCell>
                    <TableCell sx={{ py: 1 }}>{p.name}</TableCell>
                    <TableCell sx={{ py: 1 }}>
                      {p.address}:{p.port}
                    </TableCell>
                    <TableCell
                      sx={{ fontFamily: "monospace", fontSize: "0.75rem", py: 1 }}
                    >
                      {truncateKey(p.public_key)}
                    </TableCell>
                    <TableCell sx={{ py: 1 }}></TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
          </Box>
        </Box>
      </AccordionDetails>
    </Accordion>
  );
};
