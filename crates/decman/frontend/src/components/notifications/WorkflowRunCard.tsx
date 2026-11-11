import { useState } from "react";
import { Box, Button, Chip, CircularProgress, IconButton, Tooltip, Typography } from "@mui/material";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import { API_BASE } from "../../constants";
import { authenticatedFetch } from "../../api";
import { copyToClipboard } from "../../clipboard";
import { useSnackbar } from "../../contexts";
import { RowCard } from "../RowCard";
import { ApprovalCard, StatusPill } from "../viz/ApprovalCard";
import { AcsTransferMeter, WorkflowPipeline } from "../viz/WorkflowPipeline";
import { currentStepLabel, stepsForRun, workflowKindLabel } from "../../workflowSteps";
import type { WorkflowRun } from "../../types";
import { formatRelativeTime, truncatePartyId } from "./format";
import { KickedOwnerKeyRow } from "./KickedOwnerKeyRow";

const WorkflowRunCard = ({
  run,
  onAfter,
  onSelectParty,
  compact = false,
}: {
  run: WorkflowRun;
  onAfter: () => void;
  onSelectParty: (partyId: string) => void;
  compact?: boolean;
}) => {
  const [busy, setBusy] = useState(false);
  const { showSnackbar } = useSnackbar();
  const isInProgress = run.status === "inprogress";
  const isTerminal =
    run.status === "completed" ||
    run.status === "failed" ||
    run.status === "cancelled";

  const cancel = async () => {
    setBusy(true);
    try {
      // Cancel THIS card's run by instance — the per-kind endpoints pick a
      // run deterministically and may target a different concurrent run.
      const res = await authenticatedFetch(
        `${API_BASE}/workflows/${encodeURIComponent(run.instance_name)}/cancel`,
        { method: "POST" },
      );
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || "Failed to cancel");
      }
      showSnackbar(`${workflowKindLabel(run.kind)} workflow cancelled`);
      onAfter();
    } catch (err) {
      showSnackbar(
        err instanceof Error ? err.message : "Failed to cancel",
        "error",
      );
    } finally {
      setBusy(false);
    }
  };

  const dismiss = async () => {
    setBusy(true);
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/workflows/${encodeURIComponent(run.instance_name)}/dismiss`,
        { method: "POST" },
      );
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || "Failed to dismiss");
      }
      onAfter();
    } catch (err) {
      showSnackbar(
        err instanceof Error ? err.message : "Failed to dismiss",
        "error",
      );
    } finally {
      setBusy(false);
    }
  };

  const retry = async () => {
    setBusy(true);
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/workflows/${encodeURIComponent(run.instance_name)}/retry`,
        { method: "POST" },
      );
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || "Failed to retry");
      }
      showSnackbar(`Retrying ${workflowKindLabel(run.kind)} workflow`);
      onAfter();
    } catch (err) {
      showSnackbar(
        err instanceof Error ? err.message : "Failed to retry",
        "error",
      );
    } finally {
      setBusy(false);
    }
  };

  // Only the coordinator knows what the other peers did: a peer-side row never
  // gets a count and would show a permanent 0. While the coordinator waits, the
  // peers that joined are the progress — nothing "completes" a step that
  // carries no command — and from the next step on it is the peers that
  // completed the current one.
  const showsPeerCount =
    isInProgress &&
    run.role === "Coordinator" &&
    (run.expected_peers?.length ?? 0) > 0;
  const waitingForPeers = run.current_step === "WaitingForPeers";
  const peerCountLine = showsPeerCount
    ? waitingForPeers
      ? `${run.connected_peers?.length ?? 0} of ${run.expected_peers.length} peers joined`
      : `${run.completed_peers?.length ?? 0} of ${run.expected_peers.length} peers responded`
    : null;

  const fromLine = run.role === "Coordinator"
    ? "started by you"
    : run.coordinator_name
      ? `from ${run.coordinator_name}`
      : run.coordinator_pubkey
        ? `from ${run.coordinator_pubkey.slice(0, 12)}…${run.coordinator_pubkey.slice(-6)}`
        : null;

  // Terminal runs collapse to a single dense row in the Completed section.
  if (compact) {
    return (
      <RowCard
        dataAttrs={{
          "data-testid": "workflow-run-card",
          "data-kind": run.kind,
          "data-prefix": run.prefix ?? "",
          "data-status": run.status,
        }}
      >
        <Typography
          sx={{
            fontSize: 14,
            fontWeight: 500,
            whiteSpace: "nowrap",
            flexShrink: 0,
            width: 168,
          }}
        >
          {workflowKindLabel(run.kind)} workflow
        </Typography>
        {run.status === "failed" && run.error ? (
          <Box
            sx={{
              flex: 1,
              minWidth: 0,
              display: "flex",
              alignItems: "center",
              gap: 0.5,
            }}
          >
            <Tooltip title={run.error}>
              <Typography
                sx={{
                  fontSize: 12,
                  color: "error.main",
                  minWidth: 0,
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                }}
              >
                {run.error}
              </Typography>
            </Tooltip>
            <Tooltip title="Copy error">
              <IconButton
                size="small"
                onClick={async () => {
                  const ok = await copyToClipboard(run.error!);
                  showSnackbar(ok ? "Copied to clipboard" : "Failed to copy");
                }}
                sx={{ p: 0.25, flexShrink: 0 }}
              >
                <ContentCopyIcon sx={{ fontSize: 14 }} />
              </IconButton>
            </Tooltip>
          </Box>
        ) : (
          <Typography
            sx={{
              fontFamily: "var(--font-mono)",
              fontSize: 12,
              color: "text.secondary",
              flex: 1,
              minWidth: 0,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
          >
            {run.prefix ||
              (run.dec_party_id
                ? truncatePartyId(run.dec_party_id)
                : fromLine || "")}
          </Typography>
        )}
        <Box sx={{ width: 96, flexShrink: 0 }}>
          <StatusPill status={run.status} />
        </Box>
        <Typography
          sx={{
            fontFamily: "var(--font-mono)",
            fontSize: 12,
            color: "text.secondary",
            whiteSpace: "nowrap",
            width: 52,
            textAlign: "right",
            flexShrink: 0,
          }}
        >
          {formatRelativeTime(run.updated_at)}
        </Typography>
        <Box
          sx={{
            width: 128,
            flexShrink: 0,
            display: "flex",
            justifyContent: "flex-end",
            gap: 0.5,
          }}
        >
          {run.status === "failed" && run.role === "Coordinator" && (
            <Button
              variant="text"
              size="small"
              onClick={retry}
              disabled={busy}
              sx={{ minWidth: 0 }}
            >
              Retry
            </Button>
          )}
          <Button
            variant="text"
            size="small"
            onClick={dismiss}
            disabled={busy}
            sx={{ minWidth: 0, color: "text.secondary" }}
          >
            Dismiss
          </Button>
        </Box>
      </RowCard>
    );
  }

  return (
    <ApprovalCard
      dataAttrs={{
        "data-testid": "workflow-run-card",
        "data-kind": run.kind,
        "data-prefix": run.prefix ?? "",
        "data-status": run.status,
      }}
      // The pipeline, the peer count and the current step below already say a
      // run is going; a "Running" pill on top of them is noise. Queued and the
      // terminal states have no such body, so they keep theirs.
      pill={isInProgress ? undefined : <StatusPill status={run.status} />}
      time={formatRelativeTime(run.updated_at)}
      title={
        run.prefix
          ? `${workflowKindLabel(run.kind)} · ${run.prefix}`
          : workflowKindLabel(run.kind)
      }
      facts={
        <>
          {(fromLine || peerCountLine) && (
            <Typography sx={{ fontSize: 13, color: "text.secondary" }}>
              {[fromLine, peerCountLine].filter(Boolean).join(" · ")}
            </Typography>
          )}
          {isInProgress && run.step_total > 0 && (
            <Box sx={{ mt: fromLine ? 1.25 : 0 }}>
              <WorkflowPipeline
                current={run.step_index}
                total={run.step_total}
                steps={stepsForRun(run)}
              />
            </Box>
          )}
          {isInProgress && run.acs_progress && (
            <Box sx={{ mt: 1.25 }}>
              <AcsTransferMeter progress={run.acs_progress} />
            </Box>
          )}
        </>
      }
      footerLeft={
        isInProgress && run.current_step ? (
          <Typography sx={{ fontSize: 13, fontWeight: 600 }}>
            {currentStepLabel(run)}
          </Typography>
        ) : undefined
      }
      actions={
        <>
          {isInProgress && run.role === "Coordinator" && (
            <Button
              variant="outlined"
              color="error"
              size="small"
              onClick={cancel}
              disabled={busy}
              startIcon={busy ? <CircularProgress size={14} /> : undefined}
            >
              Cancel workflow
            </Button>
          )}
          {run.status === "failed" && run.role === "Coordinator" && (
            <Button
              variant="outlined"
              color="primary"
              size="small"
              onClick={retry}
              disabled={busy}
              startIcon={busy ? <CircularProgress size={14} /> : undefined}
            >
              Retry
            </Button>
          )}
          {isTerminal && (
            <Button
              variant="text"
              color="inherit"
              size="small"
              onClick={dismiss}
              disabled={busy}
            >
              Dismiss
            </Button>
          )}
        </>
      }
      detail={
        run.error ||
        run.dec_party_id ||
        run.new_threshold != null ||
        run.kicked_participant ||
        run.kicked_owner_key ||
        run.added_participant ||
        (run.participants && run.participants.length > 0) ||
        (run.package_names && run.package_names.length > 0) ||
        (run.dar_filenames && run.dar_filenames.length > 0) ? (
          <Box sx={{ display: "flex", flexDirection: "column", gap: 0.75 }}>
          {run.dec_party_id && (
            <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Dec party
              </Typography>
              <Typography
                component="span"
                variant="caption"
                onClick={() => onSelectParty(run.dec_party_id!)}
                sx={{
                  fontFamily: "var(--font-mono)",
                  color: "primary.main",
                  cursor: "pointer",
                  "&:hover": { textDecoration: "underline" },
                }}
              >
                {truncatePartyId(run.dec_party_id)}
              </Typography>
            </Box>
          )}
          {run.kicked_participant && (
            <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Kicking
              </Typography>
              <Typography
                variant="body2"
                sx={{ fontWeight: 600, fontFamily: "var(--font-mono)" }}
              >
                {truncatePartyId(run.kicked_participant)}
              </Typography>
              <Tooltip title="Copy kicked party id">
                <IconButton
                  size="small"
                  onClick={async () => {
                    const ok = await copyToClipboard(run.kicked_participant!);
                    showSnackbar(ok ? "Copied to clipboard" : "Failed to copy");
                  }}
                  sx={{ p: 0.25 }}
                >
                  <ContentCopyIcon sx={{ fontSize: 14 }} />
                </IconButton>
              </Tooltip>
            </Box>
          )}
          {run.kicked_owner_key && (
            <KickedOwnerKeyRow ownerKey={run.kicked_owner_key} />
          )}
          {run.added_participant && (
            <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Adding
              </Typography>
              <Typography
                variant="body2"
                sx={{ fontWeight: 600, fontFamily: "var(--font-mono)" }}
              >
                {truncatePartyId(run.added_participant)}
              </Typography>
              <Tooltip title="Copy added participant id">
                <IconButton
                  size="small"
                  onClick={async () => {
                    const ok = await copyToClipboard(run.added_participant!);
                    showSnackbar(ok ? "Copied to clipboard" : "Failed to copy");
                  }}
                  sx={{ p: 0.25 }}
                >
                  <ContentCopyIcon sx={{ fontSize: 14 }} />
                </IconButton>
              </Tooltip>
            </Box>
          )}
          {run.participants && run.participants.length > 0 && (
            <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Participants ({run.participants.length})
              </Typography>
              <Box sx={{ display: "flex", flexWrap: "wrap", gap: 0.5 }}>
                {run.participants.map((id) => (
                  <Chip
                    key={id}
                    size="small"
                    variant="outlined"
                    label={truncatePartyId(id)}
                  />
                ))}
              </Box>
            </Box>
          )}
          {run.package_names && run.package_names.length > 0 && (
            <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Packages ({run.package_names.length})
              </Typography>
              <Box sx={{ display: "flex", flexWrap: "wrap", gap: 0.5 }}>
                {run.package_names.map((name) => (
                  <Chip
                    key={name}
                    size="small"
                    variant="outlined"
                    label={name}
                  />
                ))}
              </Box>
            </Box>
          )}
          {run.dar_filenames && run.dar_filenames.length > 0 && (
            <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                DAR files ({run.dar_filenames.length})
              </Typography>
              <Box sx={{ display: "flex", flexWrap: "wrap", gap: 0.5 }}>
                {run.dar_filenames.map((name) => (
                  <Chip
                    key={name}
                    size="small"
                    variant="outlined"
                    label={name}
                  />
                ))}
              </Box>
            </Box>
          )}
          {run.new_threshold != null && (
            <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Threshold
              </Typography>
              <Typography variant="body2">
                {run.previous_threshold != null ? (
                  <>
                    <Box
                      component="span"
                      sx={{ color: "text.secondary" }}
                    >
                      {run.previous_threshold}
                    </Box>{" "}
                    →{" "}
                    <Box component="span" sx={{ fontWeight: 600 }}>
                      {run.new_threshold}
                    </Box>
                  </>
                ) : (
                  <Box component="span" sx={{ fontWeight: 600 }}>
                    {run.new_threshold}
                  </Box>
                )}
              </Typography>
            </Box>
          )}
          {run.error && (
            <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Error
              </Typography>
              <Typography variant="body2" color="error">
                {run.error}
              </Typography>
            </Box>
          )}
          </Box>
        ) : undefined
      }
    />
  );
};

export { WorkflowRunCard };
