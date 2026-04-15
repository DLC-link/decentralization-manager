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
  IconButton,
} from "@mui/material";
import UploadFileIcon from "@mui/icons-material/UploadFile";
import DeleteIcon from "@mui/icons-material/Delete";
import { API_BASE } from "../constants";
import type { DarsStatusResponse, DarFile } from "../types";

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
  const [status, setStatus] = useState<DarsStatusResponse | null>(null);
  const [darFiles, setDarFiles] = useState<DarFile[]>([]);

  // Reset state when dialog opens/closes
  useEffect(() => {
    if (!open) {
      setError(null);
      setStatus(null);
      setLoading(false);
      setDarFiles([]);
    }
  }, [open]);

  const pollStatus = useCallback(async () => {
    try {
      const res = await fetch(`${API_BASE}/dars/distribute/status`);
      if (res.ok) {
        const data: DarsStatusResponse = await res.json();
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
      pollStatus();
      interval = window.setInterval(pollStatus, 2000);
    }

    return () => {
      if (interval) clearInterval(interval);
    };
  }, [status?.status, pollStatus]);

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

    try {
      const endpoint =
        mode === "upload"
          ? `${API_BASE}/dars/upload`
          : `${API_BASE}/dars/distribute`;
      const res = await fetch(endpoint, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ dar_files: darFiles }),
      });

      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || "Failed to upload DARs");
      }

      if (mode === "upload") {
        // Local upload is synchronous — done immediately
        setStatus({ status: "completed" });
        setLoading(false);
        onComplete();
      } else {
        setStatus({ status: "inprogress" });
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

  const isInProgress = status?.status === "inprogress";
  const isCompleted = status?.status === "completed";
  const isFailed = status?.status === "failed";

  return (
    <Dialog open={open} onClose={handleClose} maxWidth="sm" fullWidth>
      <DialogTitle>
        {mode === "upload" ? "Upload DARs" : "Distribute DARs"}
      </DialogTitle>
      <DialogContent>
        <Box sx={{ display: "flex", flexDirection: "column", gap: 2, mt: 1 }}>
          {error && <Alert severity="error">{error}</Alert>}

          {isInProgress && (
            <Alert severity="info" icon={<CircularProgress size={20} />}>
              {mode === "upload"
                ? "Uploading DARs to this node..."
                : "Distributing DARs to all participants..."}
            </Alert>
          )}

          {isCompleted && (
            <Alert severity="success">
              {mode === "upload"
                ? "DARs uploaded to this node successfully!"
                : "DARs distributed to all participants successfully!"}
            </Alert>
          )}

          {isFailed && (
            <Alert severity="error">
              {mode === "upload" ? "Upload" : "Distribution"} failed:{" "}
              {status.error || "Unknown error"}
            </Alert>
          )}

          {!isInProgress && !isCompleted && (
            <>
              <Typography variant="body2" color="text.secondary">
                {mode === "upload"
                  ? "Upload Daml Archive (DAR) files to this node only."
                  : "Distribute Daml Archive (DAR) files to all participants. This will coordinate with other nodes via Noise protocol."}
              </Typography>

              <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
                <Button
                  component="label"
                  variant="outlined"
                  startIcon={<UploadFileIcon />}
                >
                  Select DAR Files
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
                      <IconButton
                        size="small"
                        onClick={() => handleRemoveDarFile(index)}
                      >
                        <DeleteIcon fontSize="small" />
                      </IconButton>
                    </Box>
                  ))}
                </Box>
              )}
            </>
          )}
        </Box>
      </DialogContent>
      <DialogActions>
        <Button onClick={handleClose} disabled={loading}>
          {isCompleted || isFailed ? "Close" : "Cancel"}
        </Button>
        {(!status?.status || status.status === "idle" || isFailed) ? (
          <Button
            onClick={handleStart}
            variant="contained"
            color="primary"
            disabled={loading || darFiles.length === 0}
          >
            {loading ? (
              <CircularProgress size={20} />
            ) : mode === "upload" ? (
              "Upload DARs"
            ) : (
              "Distribute DARs"
            )}
          </Button>
        ) : null}
      </DialogActions>
    </Dialog>
  );
};
