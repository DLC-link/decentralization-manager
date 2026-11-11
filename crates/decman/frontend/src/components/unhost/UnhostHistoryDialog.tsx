import { useEffect, useState } from "react";
import { Alert, Box, Button, Chip, Dialog, DialogActions, DialogContent, DialogTitle, LinearProgress, Typography } from "@mui/material";
import { authenticatedFetch } from "../../api";
import { CopyableText } from "../CopyableText";
import type { UnhostAuditEntry, UnhostAuditResponse } from "../../types";
import { IdList } from "./IdList";
import { partyPath, errorOf, type DialogProps } from "./shared";

const STATUS_COLOR: Record<string, "success" | "error" | "warning"> = {
  completed: "success",
  failed: "error",
  in_progress: "warning",
};

/** Unix seconds as a readable UTC date and time. */
const formatSecs = (secs: number | null | undefined) =>
  secs == null ? "—" : new Date(secs * 1000).toISOString().replace("T", " ").slice(0, 19) + " UTC";

const AuditRow = ({ entry }: { entry: UnhostAuditEntry }) => {
  const archived = entry.validator_rights_archived.length + entry.preapprovals_cancelled.length;
  const serials =
    entry.serial_before == null
      ? "—"
      : `${entry.serial_before} → ${entry.serial_after ?? "…"}`;
  return (
    <Box
      data-testid="unhost-audit-row"
      sx={{
        border: "1px solid",
        borderColor: "divider",
        borderRadius: "8px",
        p: 1.5,
        display: "flex",
        flexDirection: "column",
        gap: 0.75,
      }}
    >
      <Box sx={{ display: "flex", alignItems: "center", gap: 1, flexWrap: "wrap" }}>
        <Chip
          size="small"
          variant="outlined"
          label={entry.status.replace("_", " ")}
          color={STATUS_COLOR[entry.status] ?? "default"}
        />
        <Typography variant="body2">
          #{entry.id} · {formatSecs(entry.started_at)}
        </Typography>
        <Typography variant="body2" color="text.secondary">
          by {entry.requested_by} ({entry.requested_via})
        </Typography>
      </Box>
      <Typography variant="body2" color="text.secondary">
        Step {entry.step} · serial {serials} ·{" "}
        {entry.splice_cleanup ? `${archived} Splice contract(s) archived` : "Splice cleanup skipped"}
        {entry.resumed_from != null && ` · resumed #${entry.resumed_from}`}
      </Typography>
      {entry.error && (
        <Typography variant="body2" color="error.main" sx={{ wordBreak: "break-word" }}>
          {entry.error}
        </Typography>
      )}
      <IdList label="ValidatorRight archived" ids={entry.validator_rights_archived} />
      <IdList label="TransferPreapproval cancelled" ids={entry.preapprovals_cancelled} />
    </Box>
  );
};

/** Every attempt to make this node leave the party, newest first. */
export const UnhostHistoryDialog = ({ partyId, onClose }: DialogProps) =>
  partyId === null ? null : <UnhostHistory key={partyId} partyId={partyId} onClose={onClose} />;

const UnhostHistory = ({ partyId, onClose }: { partyId: string; onClose: () => void }) => {
  const [entries, setEntries] = useState<UnhostAuditEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const response = await authenticatedFetch(`${partyPath(partyId)}/unhost-audit`);
        if (!response.ok) throw new Error(await errorOf(response, "Failed to read the history"));
        const data: UnhostAuditResponse = await response.json();
        if (!cancelled) setEntries(data.entries);
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : "Failed to read the history");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [partyId]);

  return (
    <Dialog open onClose={onClose} maxWidth="sm" fullWidth>
      <DialogTitle>Unhost history</DialogTitle>
      <DialogContent sx={{ display: "flex", flexDirection: "column", gap: 1.5 }}>
        <CopyableText text={partyId} truncate={{ start: 32, end: 16 }} variant="body2" />
        {!entries && !error && <LinearProgress aria-label="Reading the history" />}
        {error && <Alert severity="error">{error}</Alert>}
        {entries?.length === 0 && (
          <Typography variant="body2" color="text.secondary">
            This node has never tried to stop hosting this party.
          </Typography>
        )}
        {entries?.map((entry) => <AuditRow key={entry.id} entry={entry} />)}
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Close</Button>
      </DialogActions>
    </Dialog>
  );
};
