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
import { zebraRow } from "../styles";
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
  /** `name` is free text and may be empty, so rows fall back to the id. */
  label: string;
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

// The theme pads a table's leading and trailing cell out to `--content-pad`,
// which is viewport-derived and reaches ~180px on a wide screen. Inside a
// dialog the dialog's own edge is the boundary, so pin the gutter instead.
const GUTTER = 24;

const tableSx = {
  tableLayout: "fixed" as const,
  "& .MuiTableCell-root": { height: 44 },
  "& .MuiTableCell-root:first-of-type": { paddingLeft: `${GUTTER}px` },
  "& .MuiTableCell-root:last-of-type": { paddingRight: `${GUTTER}px` },
};

// Only the free-text columns clip; a chip cell that inherits this loses the
// end of its own label.
const ellipsisSx = {
  overflow: "hidden",
  textOverflow: "ellipsis",
  whiteSpace: "nowrap",
} as const;

const truncateKey = (key: string): string =>
  key.length > 16 ? `${key.slice(0, 10)}…${key.slice(-4)}` : key;

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
  // Bumped per file chosen, so a slow read of an earlier file cannot land
  // after a later one and show file B while importing file A.
  const readId = useRef(0);
  const { showSnackbar } = useSnackbar();

  const isImport = mode === "import";

  const reset = () => {
    readId.current += 1;
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
            label: peer.name || peer.participant_id,
          })),
    [isImport, importRows, peers, selfNodeId],
  );

  const handleFile = async (file: File) => {
    reset();
    const read = ++readId.current;
    setFilename(file.name);
    let text: string;
    try {
      text = await file.text();
    } catch {
      if (read === readId.current) setError(`Could not read ${file.name}.`);
      return;
    }
    if (read !== readId.current) return;
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
          label: peer.name || peer.participant_id,
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
      <DialogContent dividers sx={{ p: 0 }}>
        <Box sx={{ px: `${GUTTER}px`, pt: 2, pb: rows.length === 0 ? 2 : 1 }}>
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

          <Typography variant="body2" color="text.secondary">
            {rows.length > 0
              ? `${selectedPeers.length} of ${rows.length} selected`
              : isImport
                ? filename
                  ? "No usable peers in this file."
                  : "Choose a CSV file to see the peers it contains."
                : "No peers configured yet."}
          </Typography>
        </Box>

        {rows.length > 0 && (
          <Box sx={{ pb: isImport ? 0 : 1 }}>
            <Table size="small" sx={tableSx}>
              <TableHead>
                <TableRow>
                  <TableCell padding="checkbox" sx={{ width: 56 }}>
                    <Checkbox
                      size="small"
                      checked={allSelected}
                      indeterminate={selectedPeers.length > 0 && !allSelected}
                      onChange={toggleAll}
                      disabled={saving}
                      slotProps={{
                        input: { "aria-label": "Select all peers" },
                      }}
                    />
                  </TableCell>
                  <TableCell sx={{ width: "22%" }}>Name</TableCell>
                  <TableCell>Address</TableCell>
                  <TableCell sx={{ width: 172 }}>Public Key</TableCell>
                  {isImport && (
                    <TableCell sx={{ width: 128 }}>Change</TableCell>
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
                            input: { "aria-label": `Select ${row.label}` },
                          }}
                        />
                      </TableCell>
                      <TableCell title={row.label} sx={ellipsisSx}>
                        {row.label}
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
                      {/* The port is the half worth reading, so the host
                        * clips and the port stays pinned beside it. */}
                      <TableCell title={`${row.peer.address}:${row.peer.port}`}>
                        <Box sx={{ display: "flex", minWidth: 0 }}>
                          <Box component="span" sx={ellipsisSx}>
                            {row.peer.address}
                          </Box>
                          <Box component="span" sx={{ flexShrink: 0 }}>
                            :{row.peer.port}
                          </Box>
                        </Box>
                      </TableCell>
                      <TableCell
                        title={row.peer.public_key}
                        sx={{
                          ...ellipsisSx,
                          fontFamily: "var(--font-mono)",
                          fontSize: "0.75rem",
                        }}
                      >
                        {truncateKey(row.peer.public_key)}
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
        )}

      </DialogContent>
      <DialogActions sx={{ px: `${GUTTER}px`, gap: 1 }}>
        {/* In the action bar rather than the scroll area: it is what the
          * Import button is about to do, so it must not sit below the fold. */}
        <Typography
          variant="body2"
          color="text.secondary"
          sx={{ flexGrow: 1, mr: 2 }}
        >
          {!isImport || rows.length === 0
            ? ""
            : selectedPeers.length === 0
              ? "No peers selected."
              : changedCount === 0
                ? "Nothing to change — the selected peers already match."
                : `${changedCount} peer${plural(changedCount)} to add or update. Peers not in the file are kept.`}
        </Typography>
        <Button onClick={onClose} disabled={saving}>
          Cancel
        </Button>
        <Button
          variant="contained"
          startIcon={isImport ? <UploadFileIcon /> : <DownloadIcon />}
          onClick={isImport ? handleImport : handleExport}
          disabled={
            saving || selectedPeers.length === 0 || (isImport && !onSave)
          }
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
