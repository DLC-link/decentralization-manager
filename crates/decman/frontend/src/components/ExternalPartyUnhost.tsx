import { useEffect, useRef, useState } from "react";
import {
  Alert,
  Box,
  Button,
  Checkbox,
  Chip,
  CircularProgress,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  FormControlLabel,
  LinearProgress,
  Typography,
} from "@mui/material";
import LinkOffIcon from "@mui/icons-material/LinkOff";
import { authenticatedFetch } from "../api";
import { API_BASE } from "../constants";
import { useSnackbar } from "../contexts";
import { legendSx } from "../styles";
import { CopyableText } from "./CopyableText";
import type {
  ExternalPartyUnhostPreview,
  ExternalPartyUnhostRequest,
  ExternalPartyUnhostResponse,
  UnhostAuditEntry,
  UnhostAuditResponse,
} from "../types";

const partyPath = (partyId: string) =>
  `${API_BASE}/external-parties/${encodeURIComponent(partyId)}`;

/** The server's `{ error }` message, or `fallback` when the body has none. */
const errorOf = async (response: Response, fallback: string) => {
  const body = await response.json().catch(() => null);
  return body?.error || `${fallback} (HTTP ${response.status})`;
};

/** How long the dialog waits before it repeats a pending unhost. */
export const UNHOST_RETRY_MS = 5_000;
/** How many repeats it makes: two minutes in all. */
const UNHOST_RETRIES = 24;

type Attempt = { kind: "done" } | { kind: "wait" } | { kind: "error"; message: string };

/**
 * Send the pinned unhost once, and read the answer as finished, still going, or
 * failed. The node keeps working when a request is cut short, so a dropped
 * request and a gateway timeout read as still going.
 */
const attemptUnhost = async (
  partyId: string,
  body: ExternalPartyUnhostRequest,
): Promise<Attempt> => {
  let response: Response;
  try {
    response = await authenticatedFetch(`${partyPath(partyId)}/unhost`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
  } catch {
    return { kind: "wait" };
  }
  // 202: Canton accepted the write, but it is not effective yet.
  if (response.status === 200) return { kind: "done" };
  if ([202, 502, 503, 504].includes(response.status)) return { kind: "wait" };
  if (response.status === 409) {
    const message = await errorOf(response, "Failed to stop hosting");
    // An earlier request, perhaps this dialog's own, still runs on the node.
    return message.includes("already running") ? { kind: "wait" } : { kind: "error", message };
  }
  if (response.status === 500) {
    const result: Partial<ExternalPartyUnhostResponse> = await response.json().catch(() => ({}));
    const archived =
      (result.validator_rights_archived?.length ?? 0) + (result.preapprovals_cancelled?.length ?? 0);
    return {
      kind: "error",
      message: `A step failed after ${archived} Splice contract(s) were archived. The party is still hosted here. The unhost history holds the reason.`,
    };
  }
  return { kind: "error", message: await errorOf(response, "Failed to stop hosting") };
};

/** A labelled list of contract or participant ids, or nothing when empty. */
const IdList = ({ label, ids }: { label: string; ids: string[] }) =>
  ids.length === 0 ? null : (
    <Box sx={{ display: "flex", flexDirection: "column", gap: 0.5 }}>
      <Typography component="span" sx={{ ...legendSx, fontSize: "0.65rem" }}>
        {label}
      </Typography>
      {ids.map((id) => (
        <CopyableText key={id} text={id} truncate={{ start: 24, end: 12 }} variant="body2" />
      ))}
    </Box>
  );

interface DialogProps {
  /** The party the dialog is about, or `null` when it is closed. */
  partyId: string | null;
  onClose: () => void;
}

/**
 * Confirm step for "Stop hosting". It reads the preview first, so the operator
 * sees the serial, the hosts that remain, and every Splice contract the node
 * archives before anything is written.
 *
 * Keyed on the party, so every opening starts from a fresh read.
 */
export const StopHostingDialog = ({ partyId, onClose }: DialogProps) =>
  partyId === null ? null : <StopHosting key={partyId} partyId={partyId} onClose={onClose} />;

const StopHosting = ({ partyId, onClose }: { partyId: string; onClose: () => void }) => {
  const { showSnackbar } = useSnackbar();
  const [preview, setPreview] = useState<ExternalPartyUnhostPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [skipCleanup, setSkipCleanup] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [waiting, setWaiting] = useState(false);
  // Set when the dialog closes, so a pending wait stops asking. Cleared on
  // setup too, because StrictMode runs setup, cleanup and setup again.
  const stopped = useRef(false);
  useEffect(() => {
    stopped.current = false;
    return () => {
      stopped.current = true;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const response = await authenticatedFetch(`${partyPath(partyId)}/unhost/preview`);
        if (!response.ok) throw new Error(await errorOf(response, "Failed to read the preview"));
        const data: ExternalPartyUnhostPreview = await response.json();
        if (!cancelled) setPreview(data);
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : "Failed to read the preview");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [partyId]);

  const noOperator = preview != null && !preview.validator_operator_party;
  const canConfirm =
    preview != null && !preview.blocker && !submitting && (!noOperator || skipCleanup);

  const confirm = async () => {
    if (!preview) return;
    setSubmitting(true);
    setError(null);
    const body: ExternalPartyUnhostRequest = {
      base_serial: preview.serial,
      // Omitted runs the cleanup. Without an operator party the node can only
      // skip it, and only because the operator said so above.
      ...(noOperator ? { splice_cleanup: false } : {}),
    };
    // The same pinned request every time. Once the node is gone the node
    // answers 200 to it, whatever the preview would now say.
    for (let attempt = 0; attempt <= UNHOST_RETRIES; attempt++) {
      if (attempt > 0) {
        setWaiting(true);
        await new Promise((resolve) => setTimeout(resolve, UNHOST_RETRY_MS));
      }
      if (stopped.current) return;
      const outcome = await attemptUnhost(partyId, body);
      if (stopped.current) return;
      if (outcome.kind === "done") {
        showSnackbar(`This node no longer hosts ${partyId}`);
        onClose();
        return;
      }
      if (outcome.kind === "error") {
        setError(outcome.message);
        setWaiting(false);
        setSubmitting(false);
        return;
      }
    }
    setError(
      "The change has not taken effect after two minutes. The node keeps working on it, and the unhost history records the result.",
    );
    setWaiting(false);
    setSubmitting(false);
  };

  const nothingToArchive =
    preview != null &&
    preview.validator_rights.length === 0 &&
    preview.preapprovals_to_cancel.length === 0;

  return (
    <Dialog open onClose={() => (!submitting || waiting) && onClose()} maxWidth="sm" fullWidth>
      <DialogTitle>Stop hosting this party</DialogTitle>
      <DialogContent sx={{ display: "flex", flexDirection: "column", gap: 2 }}>
        <CopyableText text={partyId} truncate={{ start: 32, end: 16 }} variant="body2" />
        {!preview && !error && <LinearProgress aria-label="Reading the preview" />}
        {preview && (
          <>
            <Typography variant="body2">
              This node removes itself from the party's hosts with its own key. The party's key is
              not needed. The party keeps its threshold of {preview.threshold}, with{" "}
              {preview.remaining_hosts.length} host(s) left at serial {preview.serial + 1}.
            </Typography>
            <IdList label="Hosts that remain" ids={preview.remaining_hosts} />
            {preview.blocker && <Alert severity="error">{preview.blocker}</Alert>}
            {noOperator ? (
              <>
                <Alert severity="info">
                  This node has no validator operator party (DECPM_VALIDATOR_OPERATOR_PARTY), so it
                  cannot archive the party's Splice contracts.
                </Alert>
                <FormControlLabel
                  control={
                    <Checkbox
                      checked={skipCleanup}
                      onChange={(e) => setSkipCleanup(e.target.checked)}
                    />
                  }
                  label="This node never acted as the party's Splice validator. Leave without the Splice cleanup."
                />
              </>
            ) : (
              <>
                <Typography variant="body2">
                  {nothingToArchive
                    ? "This node's validator holds no Splice contracts for the party, so nothing is archived."
                    : "These Splice contracts are archived first, as the validator operator:"}
                </Typography>
                <IdList label="ValidatorRight" ids={preview.validator_rights} />
                <IdList label="TransferPreapproval" ids={preview.preapprovals_to_cancel} />
                <IdList
                  label="Expired TransferPreapproval, left for the DSO"
                  ids={preview.preapprovals_expired}
                />
              </>
            )}
            <Typography variant="body2" color="text.secondary">
              The party's contracts stay in this node's store.
            </Typography>
          </>
        )}
        {waiting && (
          <>
            <Alert severity="info">
              The node is still leaving the party. This dialog asks again every few seconds until it
              is gone.
            </Alert>
            <LinearProgress aria-label="Waiting for the node to leave" />
          </>
        )}
        {error && <Alert severity="error">{error}</Alert>}
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose} disabled={submitting && !waiting}>
          Cancel
        </Button>
        <Button
          variant="contained"
          color="error"
          onClick={confirm}
          disabled={!canConfirm}
          startIcon={submitting ? <CircularProgress size={16} /> : <LinkOffIcon />}
        >
          Stop hosting
        </Button>
      </DialogActions>
    </Dialog>
  );
};

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
