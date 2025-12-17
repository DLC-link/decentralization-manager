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
} from "@mui/material";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import CircleIcon from "@mui/icons-material/Circle";
import EditIcon from "@mui/icons-material/Edit";
import DeleteIcon from "@mui/icons-material/Delete";
import AddIcon from "@mui/icons-material/Add";
import SaveIcon from "@mui/icons-material/Save";
import CancelIcon from "@mui/icons-material/Cancel";
import type { NetworkConfig, Peer, ParticipantStatus } from "../types";

const accordionSx = {
  borderRadius: 3,
  mb: 2,
  "&:first-of-type": { borderRadius: 3 },
  "&:last-of-type": { borderRadius: 3 },
  overflow: "hidden",
};

interface NetworkConfigAccordionProps {
  config: NetworkConfig;
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
  participantStatuses,
  onSave,
}: NetworkConfigAccordionProps) => {
  const [editing, setEditing] = useState(false);
  const [editedPeers, setEditedPeers] = useState<Peer[]>([]);
  const [saving, setSaving] = useState(false);

  const getStatus = (id: string) =>
    participantStatuses?.find((s) => s.id === id)?.active;

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

  const updatePeer = (index: number, field: keyof Peer, value: string | number) => {
    setEditedPeers((peers) =>
      peers.map((p, i) => (i === index ? { ...p, [field]: value } : p))
    );
  };

  const addPeer = () => {
    setEditedPeers((peers) => [...peers, { ...emptyPeer }]);
  };

  const removePeer = (index: number) => {
    setEditedPeers((peers) => peers.filter((_, i) => i !== index));
  };

  if (editing) {
    return (
      <Accordion sx={accordionSx} defaultExpanded>
        <AccordionSummary
          expandIcon={<ExpandMoreIcon />}
          sx={{ borderRadius: "12px 12px 0 0" }}
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
                  onChange={(e) => updatePeer(index, "port", parseInt(e.target.value) || 0)}
                />
                <TextField
                  size="small"
                  label="Public Key"
                  value={peer.public_key}
                  onChange={(e) => updatePeer(index, "public_key", e.target.value)}
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
            <Box sx={{ display: "flex", gap: 1, justifyContent: "space-between" }}>
              <Button
                startIcon={<AddIcon />}
                onClick={addPeer}
                variant="outlined"
                size="small"
              >
                Add Peer
              </Button>
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
        sx={{ borderRadius: "12px 12px 0 0" }}
      >
        <Typography variant="h6">Network Configuration</Typography>
      </AccordionSummary>
      <AccordionDetails sx={{ p: 3 }}>
        <Box>
          <Box sx={{ display: "flex", justifyContent: "space-between", mb: 1 }}>
            <Typography variant="subtitle1">Peers:</Typography>
            {onSave && (
              <IconButton size="small" onClick={startEditing}>
                <EditIcon fontSize="small" />
              </IconButton>
            )}
          </Box>
          <Table size="small">
            <TableHead>
              <TableRow>
                <TableCell>Status</TableCell>
                <TableCell>ID</TableCell>
                <TableCell>Name</TableCell>
                <TableCell>Address</TableCell>
                <TableCell>Public Key</TableCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {config.peers.map((p) => {
                const isActive = getStatus(p.id);
                return (
                  <TableRow key={p.id}>
                    <TableCell>
                      <CircleIcon
                        sx={{
                          fontSize: 12,
                          color:
                            isActive === undefined
                              ? "text.disabled"
                              : isActive
                                ? "success.main"
                                : "error.main",
                        }}
                      />
                    </TableCell>
                    <TableCell>{p.id}</TableCell>
                    <TableCell>{p.name}</TableCell>
                    <TableCell>
                      {p.address}:{p.port}
                    </TableCell>
                    <TableCell sx={{ fontFamily: "monospace", fontSize: "0.75rem" }}>
                      {p.public_key.slice(0, 16)}...
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </Box>
      </AccordionDetails>
    </Accordion>
  );
};
