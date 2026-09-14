import { useMemo, useRef, useState } from "react";
import {
  Alert,
  AlertTitle,
  Box,
  Button,
  Checkbox,
  Chip,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  Divider,
  FormControlLabel,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  Typography,
} from "@mui/material";
import DownloadIcon from "@mui/icons-material/Download";
import UploadFileIcon from "@mui/icons-material/UploadFile";
import { useSnackbar } from "../contexts";
import { cardTableSx, zebraRow } from "../styles";
import {
  downloadCsv,
  mergePeers,
  parsePeersCsv,
  peersToCsv,
  type RejectedPeerRow,
} from "../peerCsv";
import type { Peer } from "../types";

export type PeersCsvMode = "export" | "import";

interface PeersCsvDialogProps {
  open: boolean;
  mode: PeersCsvMode;
  /** The peers this dialog works on: the export set, or the import merge base. */
  peers: Peer[];
  selfNodeId?: string;
  onClose: () => void;
  /** Import only: persists the merged peer list. */
  onSave?: (peers: Peer[]) => Promise<void>;
}

type RowKind = "new" | "update" | "unchanged";

interface Row {
  peer: Peer;
  kind: RowKind;
  isSelf: boolean;
}

const samePeer = (a: Peer, b: Peer): boolean =>
  a.name === b.name &&
  a.address === b.address &&
  a.port === b.port &&
  a.public_key === b.public_key &&
  (a.party ?? "") === (b.party ?? "");

const kindChip: Record<
  RowKind,
  { label: string; color: "success" | "warning" | "default" }
> = {
  new: { label: "New", color: "success" },
  update: { label: "Update", color: "warning" },
  unchanged: { label: "Unchanged", color: "default" },
};

const exportFilename = (): string =>
  `decman-peers-${new Date().toISOString().slice(0, 10)}.csv`;

const plural = (n: number): string => (n === 1 ? "" : "s");

/**
 * Peer list transfer as CSV: `export` downloads the selected peers, `import`
 * merges the selected rows of an uploaded file into the peers table.
 *
 * Selection is held as the set of *de*selected rows, so "everything" is the
 * default without seeding state from props.
 */
export const PeersCsvDialog = ({
  open,
  mode,
  peers,
  selfNodeId,
  onClose,
  onSave,
}: PeersCsvDialogProps) => {
  const [importRows, setImportRows] = useState<Row[]>([]);
  const [rejected, setRejected] = useState<RejectedPeerRow[]>([]);
  const [deselected, setDeselected] = useState<Set<string>>(new Set());
  const [filename, setFilename] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);
  const { showSnackbar } = useSnackbar();

  const isImport = mode === "import";

  const reset = () => {
    setImportRows([]);
    setRejected([]);
    setDeselected(new Set());
    setFilename(null);
    setError(null);
    setSaving(false);
  };

  const rows: Row[] = useMemo(
    () =>
      isImport
        ? importRows
        : peers.map((peer) => ({
            peer,
            kind: "unchanged" as const,
            isSelf: peer.participant_id === selfNodeId,
          })),
    [isImport, importRows, peers, selfNodeId],
  );

  const handleFile = async (file: File) => {
    reset();
    setFilename(file.name);
    let text: string;
    try {
      text = await file.text();
    } catch {
      setError(`Could not read ${file.name}.`);
      return;
    }
    const parsed = parsePeersCsv(text);
    const existing = new Map(peers.map((p) => [p.participant_id, p]));
    setImportRows(
      parsed.rows.map(({ peer }) => {
        const current = existing.get(peer.participant_id);
        return {
          peer,
          kind: !current
            ? "new"
            : samePeer(current, peer)
              ? "unchanged"
              : "update",
          isSelf: peer.participant_id === selfNodeId,
        };
      }),
    );
    setRejected(parsed.rejected);
  };

  const toggle = (id: string) =>
    setDeselected((prev) => {
      const next = new Set(prev);
      if (!next.delete(id)) next.add(id);
      return next;
    });

  const selectedPeers = useMemo(
    () =>
      rows
        .filter((r) => !deselected.has(r.peer.participant_id))
        .map((r) => r.peer),
    [rows, deselected],
  );

  const allSelected = rows.length > 0 && selectedPeers.length === rows.length;

  const toggleAll = () =>
    setDeselected(
      allSelected ? new Set(rows.map((r) => r.peer.participant_id)) : new Set(),
    );

  const changedCount = useMemo(
    () =>
      rows.filter(
        (r) => r.kind !== "unchanged" && !deselected.has(r.peer.participant_id),
      ).length,
    [rows, deselected],
  );

  const handleExport = () => {
    downloadCsv(exportFilename(), peersToCsv(selectedPeers));
    showSnackbar(
      `Exported ${selectedPeers.length} peer${plural(selectedPeers.length)}`,
    );
    onClose();
  };

  const handleImport = async () => {
    if (!onSave) return;
    setSaving(true);
    try {
      await onSave(mergePeers(peers, selectedPeers));
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to save peers");
    } finally {
      setSaving(false);
    }
  };

  return (
    <Dialog
      open={open}
      onClose={saving ? undefined : onClose}
      maxWidth="md"
      fullWidth
      slotProps={{ transition: { onEnter: reset } }}
    >
      <DialogTitle>
        {isImport ? "Import peers from CSV" : "Export peers as CSV"}
      </DialogTitle>
      <DialogContent dividers>
        {isImport && (
          <Box sx={{ mb: 2 }}>
            <input
              ref={fileInput}
              type="file"
              accept=".csv,text/csv"
              hidden
              onChange={(e) => {
                const file = e.target.files?.[0];
                if (file) void handleFile(file);
                // Clear the value so re-picking the same file fires onChange.
                e.target.value = "";
              }}
            />
            <Button
              variant="outlined"
              size="small"
              startIcon={<UploadFileIcon />}
              onClick={() => fileInput.current?.click()}
              disabled={saving}
            >
              {filename ? "Choose a different file" : "Choose CSV file"}
            </Button>
            {filename && (
              <Typography
                component="span"
                variant="body2"
                color="text.secondary"
                sx={{ ml: 2 }}
              >
                {filename}
              </Typography>
            )}
          </Box>
        )}

        {error && (
          <Alert severity="error" sx={{ mb: 2 }}>
            {error}
          </Alert>
        )}

        {rejected.length > 0 && (
          <Alert severity="warning" sx={{ mb: 2 }}>
            <AlertTitle>
              {rejected.length} row{plural(rejected.length)} skipped
            </AlertTitle>
            {rejected.map((r) => (
              <Typography key={r.line} variant="body2">
                Line {r.line}: {r.reason}
              </Typography>
            ))}
          </Alert>
        )}

        {rows.length === 0 ? (
          <Typography variant="body2" color="text.secondary">
            {isImport
              ? filename
                ? "No usable peers in this file."
                : "Choose a CSV file to see the peers it contains."
              : "No peers configured yet."}
          </Typography>
        ) : (
          <>
            <FormControlLabel
              control={
                <Checkbox
                  size="small"
                  checked={allSelected}
                  indeterminate={selectedPeers.length > 0 && !allSelected}
                  onChange={toggleAll}
                  disabled={saving}
                />
              }
              label={`${selectedPeers.length} of ${rows.length} selected`}
            />
            <Divider />
            <Box sx={{ overflowX: "auto" }}>
              <Table size="small" sx={{ ...cardTableSx, minWidth: 640 }}>
                <TableHead>
                  <TableRow>
                    <TableCell padding="checkbox" />
                    <TableCell sx={{ whiteSpace: "nowrap" }}>Name</TableCell>
                    <TableCell sx={{ whiteSpace: "nowrap" }}>Address</TableCell>
                    <TableCell sx={{ whiteSpace: "nowrap" }}>
                      Public Key
                    </TableCell>
                    {isImport && (
                      <TableCell sx={{ whiteSpace: "nowrap" }}>Change</TableCell>
                    )}
                  </TableRow>
                </TableHead>
                <TableBody>
                  {rows.map((row, idx) => {
                    const id = row.peer.participant_id;
                    const chip = kindChip[row.kind];
                    return (
                      <TableRow key={id} sx={zebraRow(idx)}>
                        <TableCell padding="checkbox">
                          <Checkbox
                            size="small"
                            checked={!deselected.has(id)}
                            onChange={() => toggle(id)}
                            disabled={saving}
                            slotProps={{
                              input: {
                                "aria-label": `Select ${row.peer.name}`,
                              },
                            }}
                          />
                        </TableCell>
                        <TableCell sx={{ whiteSpace: "nowrap" }}>
                          {row.peer.name}
                          {row.isSelf && (
                            <Typography
                              component="span"
                              variant="caption"
                              color="text.secondary"
                              sx={{ ml: 1 }}
                            >
                              (You)
                            </Typography>
                          )}
                        </TableCell>
                        <TableCell sx={{ whiteSpace: "nowrap" }}>
                          {row.peer.address}:{row.peer.port}
                        </TableCell>
                        <TableCell
                          sx={{
                            fontFamily: "var(--font-mono)",
                            fontSize: "0.75rem",
                            maxWidth: 220,
                            overflow: "hidden",
                            textOverflow: "ellipsis",
                          }}
                        >
                          {row.peer.public_key}
                        </TableCell>
                        {isImport && (
                          <TableCell>
                            <Chip
                              size="small"
                              label={chip.label}
                              color={chip.color}
                              variant={
                                row.kind === "unchanged" ? "outlined" : "filled"
                              }
                              sx={{ height: 20, fontSize: "0.7rem" }}
                            />
                          </TableCell>
                        )}
                      </TableRow>
                    );
                  })}
                </TableBody>
              </Table>
            </Box>
            {isImport && (
              <Typography variant="body2" color="text.secondary" sx={{ mt: 2 }}>
                {changedCount === 0
                  ? "The selected peers match what is already configured — importing changes nothing."
                  : `${changedCount} peer${plural(changedCount)} will be added or updated. Peers missing from this file are kept.`}
              </Typography>
            )}
          </>
        )}
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose} disabled={saving}>
          Cancel
        </Button>
        <Button
          variant="contained"
          startIcon={isImport ? <UploadFileIcon /> : <DownloadIcon />}
          onClick={isImport ? handleImport : handleExport}
          disabled={saving || selectedPeers.length === 0 || (isImport && !onSave)}
        >
          {isImport
            ? saving
              ? "Importing..."
              : `Import ${selectedPeers.length}`
            : `Download ${selectedPeers.length}`}
        </Button>
      </DialogActions>
    </Dialog>
  );
};
