import { useState, useEffect } from "react";
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
  IconButton,
  Tooltip,
  Divider,
  FormGroup,
  FormControlLabel,
  Checkbox,
} from "@mui/material";
import UploadFileIcon from "@mui/icons-material/UploadFile";
import DeleteIcon from "@mui/icons-material/Delete";
import { API_BASE } from "../constants";
import { authenticatedFetch } from "../api";
import { useSnackbar } from "../contexts";
import { TextHelp } from "./FieldHelp";
import type { DarFile, Peer, NodeConfig } from "../types";

interface DarsDialogProps {
  open: boolean;
  onClose: () => void;
  onComplete: () => void;
  /** "upload" = local node only, "distribute" = all peers (default) */
  mode?: "upload" | "distribute";
}

export const DarsDialog = ({
  open,
  onClose,
  onComplete,
  mode = "distribute",
}: DarsDialogProps) => {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Local-only "upload finished" flag for synchronous upload mode. Distribute
  // mode does not use this — its progress is followed in the notifications
  // feed via /workflows.
  const [uploadCompleted, setUploadCompleted] = useState(false);
  const [darFiles, setDarFiles] = useState<DarFile[]>([]);
  const [peers, setPeers] = useState<Peer[]>([]);
  const [selfNodeId, setSelfNodeId] = useState<string | null>(null);
  const [selectedPeerIds, setSelectedPeerIds] = useState<Set<string>>(new Set());
  const [loadingPeers, setLoadingPeers] = useState(false);
  const { showSnackbar } = useSnackbar();
  const workflowLabel = mode === "upload" ? "Upload" : "Distribution";

  // Fetch peers when dialog opens (distribute mode only)
  useEffect(() => {
    if (!open || mode !== "distribute") return;

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
          // Default to all peers selected, excluding self
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
  }, [open, mode]);

  // Reset state when dialog opens/closes
  useEffect(() => {
    if (!open) {
      setError(null);
      setUploadCompleted(false);
      setLoading(false);
      setDarFiles([]);
      setPeers([]);
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

  // Filter out self from peer list (compare full canton ids).
  const selectablePeers = peers.filter(
    (p) => p.participant_id !== selfNodeId,
  );

  const handleFileSelect = async (
    event: React.ChangeEvent<HTMLInputElement>,
  ) => {
    const files = event.target.files;
    if (!files) return;

    const newDarFiles: DarFile[] = [];

    for (let i = 0; i < files.length; i++) {
      const file = files[i];
      if (file.name.endsWith(".dar")) {
        const arrayBuffer = await file.arrayBuffer();
        const base64 = btoa(
          new Uint8Array(arrayBuffer).reduce(
            (data, byte) => data + String.fromCharCode(byte),
            "",
          ),
        );
        newDarFiles.push({
          filename: file.name,
          data: base64,
        });
      }
    }

    setDarFiles([...darFiles, ...newDarFiles]);
    event.target.value = "";
  };

  const handleRemoveDarFile = (index: number) => {
    setDarFiles(darFiles.filter((_, i) => i !== index));
  };

  const handleStart = async () => {
    setLoading(true);
    setError(null);

    if (darFiles.length === 0) {
      setError("Please select at least one DAR file");
      setLoading(false);
      return;
    }

    if (mode === "distribute" && selectedPeerIds.size === 0) {
      setError("At least one peer must be selected");
      setLoading(false);
      return;
    }

    try {
      const endpoint =
        mode === "upload"
          ? `${API_BASE}/dars/upload`
          : `${API_BASE}/dars/distribute`;
      const body =
        mode === "upload"
          ? { dar_files: darFiles }
          : { dar_files: darFiles, peer_ids: Array.from(selectedPeerIds) };
      const res = await authenticatedFetch(endpoint, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });

      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || "Failed to upload DARs");
      }

      if (mode === "upload") {
        // Local upload is synchronous — done immediately
        setUploadCompleted(true);
        setLoading(false);
        onComplete();
      } else {
        showSnackbar(`${workflowLabel} workflow started — follow progress in the feed`);
        onClose();
      }
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
      <DialogTitle>
        {mode === "upload" ? "Upload DARs" : "Distribute DARs"}
      </DialogTitle>
      <DialogContent>
        <Box sx={{ display: "flex", flexDirection: "column", gap: 2, mt: 1 }}>
          {error && (
            <Alert severity="error" onClose={() => setError(null)}>
              {error}
            </Alert>
          )}

          {uploadCompleted && mode === "upload" && (
            <Alert severity="success">
              DARs uploaded to this node successfully!
            </Alert>
          )}

          {!uploadCompleted && (
            <>
              <Typography variant="body2" color="text.secondary">
                {mode === "upload"
                  ? "Upload Daml Archive (DAR) files to this node only."
                  : "Distribute Daml Archive (DAR) files to selected peers. This will coordinate with the chosen nodes via Noise protocol."}
              </Typography>

              <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
                <Button
                  component="label"
                  variant="outlined"
                  startIcon={<UploadFileIcon />}
                >
                  <TextHelp text="Pick one or more Daml Archive (.dar) files from your machine. These will be uploaded to the participant and, in distribute mode, sent to the peers you select below.">
                    Select DAR Files
                  </TextHelp>
                  <input
                    type="file"
                    hidden
                    multiple
                    accept=".dar"
                    onChange={handleFileSelect}
                  />
                </Button>
                <Typography variant="body2" color="text.secondary">
                  {darFiles.length === 0
                    ? "No files selected"
                    : `${darFiles.length} file${darFiles.length === 1 ? "" : "s"} selected`}
                </Typography>
              </Box>
              {darFiles.length > 0 && (
                <Box
                  sx={{
                    display: "flex",
                    flexWrap: "wrap",
                    gap: 1,
                    p: 1,
                    border: "1px solid",
                    borderColor: "divider",
                    borderRadius: 1,
                  }}
                >
                  {darFiles.map((file, index) => (
                    <Box
                      key={index}
                      sx={{
                        display: "flex",
                        alignItems: "center",
                        gap: 0.5,
                        px: 1,
                        py: 0.5,
                        bgcolor: "action.hover",
                        borderRadius: 1,
                      }}
                    >
                      <Typography variant="body2">{file.filename}</Typography>
                      <Tooltip title="Remove file">
                        <IconButton
                          size="small"
                          onClick={() => handleRemoveDarFile(index)}
                        >
                          <DeleteIcon fontSize="small" />
                        </IconButton>
                      </Tooltip>
                    </Box>
                  ))}
                </Box>
              )}

              {mode === "distribute" && (
                <>
                  <Divider />
                  <Box>
                    <Box sx={{ display: "flex", alignItems: "center", gap: 0.5, mb: 1 }}>
                      <Typography variant="subtitle2">
                        <TextHelp text="The other participants that should receive these DARs. All known peers are selected by default — uncheck any you want to skip.">
                          Select Peers to Distribute To
                        </TextHelp>
                      </Typography>
                    </Box>
                    {loadingPeers ? (
                      <Box sx={{ display: "flex", justifyContent: "center", py: 2 }}>
                        <CircularProgress size={24} />
                      </Box>
                    ) : selectablePeers.length === 0 ? (
                      <Typography variant="body2" color="text.secondary">
                        No peers configured. Add peers in the Network
                        Configuration first.
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
                                disabled={loading}
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
                </>
              )}
            </>
          )}
        </Box>
      </DialogContent>
      <DialogActions>
        <Button onClick={handleClose} disabled={loading}>
          {uploadCompleted ? "Close" : "Cancel"}
        </Button>
        {!uploadCompleted && (
          <Button
            onClick={handleStart}
            variant="contained"
            color="primary"
            disabled={
              loading ||
              darFiles.length === 0 ||
              (mode === "distribute" && selectedPeerIds.size === 0)
            }
          >
            {loading ? (
              <CircularProgress size={20} />
            ) : mode === "upload" ? (
              "Upload DARs"
            ) : (
              "Distribute DARs"
            )}
          </Button>
        )}
      </DialogActions>
    </Dialog>
  );
};
