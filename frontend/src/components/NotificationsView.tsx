import { Fragment, useState, type ReactNode } from "react";
import {
  Alert,
  Box,
  Button,
  Chip,
  CircularProgress,
  IconButton,
  Skeleton,
  Tooltip,
  Typography,
} from "@mui/material";
import TimerOffIcon from "@mui/icons-material/TimerOff";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import { API_BASE } from "../constants";
import { authenticatedFetch } from "../api";
import { copyToClipboard } from "../clipboard";
import { useSnackbar } from "../contexts";
import { formatActionDetails, formatActionType } from "../governanceFormat";
import { ExecuteDialog } from "./ExecuteDialog";
import {
  ApprovalCard,
  ConfirmRing,
  Pill,
  StatusPill,
  WorkflowPipeline,
} from "./viz/ApprovalViz";
import type {
  CancelConfirmationRequest,
  ConfirmActionRequest,
  DisclosedContractInput,
  DomainGovernanceAction,
  ExecuteActionRequest,
  ExpireConfirmationRequest,
  GovernanceAction,
  GovernanceType,
  PendingInvitation,
  WorkflowRun,
} from "../types";

export interface PartyActions {
  partyId: string;
  /** Contract that holds the GovernanceRules (or vault gov rules) — needed for confirm/execute. */
  rulesContractId?: string;
  /** Caller's member party id for this dec party — used to detect own confirmations. */
  memberPartyId?: string;
  /** Used when sending mutating requests (vault vs core_self vs core_domain). */
  governanceType: GovernanceType;
  threshold: number;
  actions: GovernanceAction[];
  /** On-chain DSO governance proposals (governance_type = "core_domain"). Surfaced
   *  as cards in the notification feed alongside off-chain actions. */
  domainActions: DomainGovernanceAction[];
}

interface NotificationsViewProps {
  pendingInvitations: PendingInvitation[];
  partyActions: PartyActions[];
  /** Live + recently-terminal workflow runs (coordinator-side for now). */
  workflowRuns: WorkflowRun[];
  /** True while any feed source is still loading its initial response. */
  loading: boolean;
  onInvitationsChanged: () => void;
  onActionsChanged: () => void;
  onWorkflowsChanged: () => void;
  onSelectParty: (partyId: string) => void;
}

const NotificationSkeleton = () => (
  <Box
    sx={{
      p: 2,
      border: 1,
      borderColor: "divider",
      borderRadius: 2,
      display: "flex",
      flexDirection: "column",
      gap: 1.25,
    }}
  >
    <Box
      sx={{
        display: "flex",
        justifyContent: "space-between",
        alignItems: "flex-start",
        gap: 1,
      }}
    >
      <Box sx={{ display: "flex", flexDirection: "column", gap: 0.5 }}>
        <Skeleton variant="text" width={180} height={22} />
        <Skeleton variant="text" width={240} height={16} />
      </Box>
      <Skeleton variant="rounded" width={96} height={22} />
    </Box>
    <Skeleton variant="rounded" height={56} />
    <Box sx={{ display: "flex", gap: 1, justifyContent: "flex-end" }}>
      <Skeleton variant="rounded" width={64} height={30} />
      <Skeleton variant="rounded" width={80} height={30} />
    </Box>
  </Box>
);

const formatRelativeTime = (epochSeconds: number): string => {
  const seconds = Math.max(0, Math.floor(Date.now() / 1000) - epochSeconds);
  if (seconds < 60) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
};

const truncatePartyId = (id: string): string => {
  const parts = id.split("::");
  if (parts.length !== 2) return id;
  const [prefix, namespace] = parts;
  return `${prefix}::${namespace.slice(0, 6)}…${namespace.slice(-6)}`;
};

const InvitationCard = ({
  invitation,
  onAfter,
  onSelectParty,
}: {
  invitation: PendingInvitation;
  onAfter: () => void;
  onSelectParty: (partyId: string) => void;
}) => {
  const [busy, setBusy] = useState(false);
  const { showSnackbar } = useSnackbar();

  const respond = async (path: "accept" | "decline") => {
    setBusy(true);
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/invitations/${path}`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ id: invitation.id }),
        },
      );
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || `Failed to ${path}`);
      }
      showSnackbar(
        path === "accept"
          ? "Invitation accepted — workflow started"
          : "Invitation declined",
      );
      onAfter();
    } catch (err) {
      showSnackbar(
        err instanceof Error ? err.message : `Failed to ${path}`,
        "error",
      );
    } finally {
      setBusy(false);
    }
  };

  const fromLabel =
    invitation.coordinator_name ||
    `${invitation.coordinator_pubkey.slice(0, 12)}…${invitation.coordinator_pubkey.slice(-6)}`;
  // Render every detail the invite payload carried, regardless of workflow
  // type — mirrors WorkflowRunCard so the peer's invitation card is as rich
  // as the coordinator's run card.
  const showMeta =
    !!invitation.prefix ||
    !!invitation.dec_party_id ||
    !!invitation.kicked_participant ||
    invitation.new_threshold != null ||
    (invitation.participants?.length ?? 0) > 0 ||
    (invitation.package_names?.length ?? 0) > 0 ||
    (invitation.dar_filenames?.length ?? 0) > 0;

  return (
    <ApprovalCard
      glyph="✉"
      eyebrow="Invitation"
      accent
      pill={<Pill label="Respond" tone="accent" />}
      time={formatRelativeTime(invitation.received_at)}
      title={`${invitation.invitation_type} invitation`}
      facts={
        <Box
          sx={{
            display: "flex",
            alignItems: "center",
            gap: 0.5,
            fontSize: 13,
            color: "text.secondary",
          }}
        >
          from
          <Box
            component="span"
            sx={{
              fontFamily: "var(--font-mono)",
              fontSize: 12,
              color: "text.primary",
            }}
          >
            {fromLabel}
          </Box>
        </Box>
      }
      actions={
        <>
          <Button
            variant="text"
            size="small"
            onClick={() => respond("decline")}
            disabled={busy}
            sx={{ color: "text.secondary" }}
          >
            Decline
          </Button>
          <Button
            variant="contained"
            color="primary"
            size="small"
            onClick={() => respond("accept")}
            disabled={busy}
            startIcon={busy ? <CircularProgress size={14} /> : undefined}
          >
            Accept
          </Button>
        </>
      }
      detail={
        showMeta ? (
          <Box
            sx={{
              display: "flex",
              flexDirection: "column",
              gap: 0.75,
            }}
          >
          {invitation.prefix && (
            <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Prefix
              </Typography>
              <Typography variant="body2" sx={{ fontWeight: 600 }}>
                {invitation.prefix}
              </Typography>
            </Box>
          )}
          {invitation.dec_party_id && (
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
                onClick={() => onSelectParty(invitation.dec_party_id!)}
                sx={{
                  fontFamily: "var(--font-mono)",
                  color: "primary.main",
                  cursor: "pointer",
                  "&:hover": { textDecoration: "underline" },
                }}
              >
                {truncatePartyId(invitation.dec_party_id)}
              </Typography>
            </Box>
          )}
          {invitation.kicked_participant && (
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
                {truncatePartyId(invitation.kicked_participant)}
              </Typography>
              <Tooltip title="Copy kicked party id">
                <IconButton
                  size="small"
                  onClick={async () => {
                    const ok = await copyToClipboard(invitation.kicked_participant!);
                    showSnackbar(ok ? "Copied to clipboard" : "Failed to copy");
                  }}
                  sx={{ p: 0.25 }}
                >
                  <ContentCopyIcon sx={{ fontSize: 14 }} />
                </IconButton>
              </Tooltip>
            </Box>
          )}
          {invitation.new_threshold != null && (
            <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Threshold
              </Typography>
              <Typography variant="body2">
                {invitation.previous_threshold != null ? (
                  <>
                    <Box
                      component="span"
                      sx={{ color: "text.secondary", textDecoration: "line-through" }}
                    >
                      {invitation.previous_threshold}
                    </Box>{" "}
                    →{" "}
                    <Box component="span" sx={{ fontWeight: 600 }}>
                      {invitation.new_threshold}
                    </Box>
                  </>
                ) : (
                  <Box component="span" sx={{ fontWeight: 600 }}>
                    {invitation.new_threshold}
                  </Box>
                )}
              </Typography>
            </Box>
          )}
          {invitation.participants && invitation.participants.length > 0 && (
            <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Participants ({invitation.participants.length})
              </Typography>
              <Box sx={{ display: "flex", flexWrap: "wrap", gap: 0.5 }}>
                {invitation.participants.map((id) => (
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
          {invitation.package_names && invitation.package_names.length > 0 && (
            <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Packages ({invitation.package_names.length})
              </Typography>
              <Box sx={{ display: "flex", flexWrap: "wrap", gap: 0.5 }}>
                {invitation.package_names.map((name) => (
                  <Chip key={name} size="small" variant="outlined" label={name} />
                ))}
              </Box>
            </Box>
          )}
          {invitation.dar_filenames && invitation.dar_filenames.length > 0 && (
            <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                DARs ({invitation.dar_filenames.length})
              </Typography>
              <Box sx={{ display: "flex", flexWrap: "wrap", gap: 0.5 }}>
                {invitation.dar_filenames.map((filename) => (
                  <Chip
                    key={filename}
                    size="small"
                    variant="outlined"
                    label={filename}
                  />
                ))}
              </Box>
            </Box>
          )}
          </Box>
        ) : undefined
      }
    />
  );
};

const ActionCard = ({
  party,
  action,
  onAfter,
  onSelectParty,
}: {
  party: PartyActions;
  action: GovernanceAction;
  onAfter: () => void;
  onSelectParty: (partyId: string) => void;
}) => {
  const [busy, setBusy] = useState(false);
  const [executeDialogOpen, setExecuteDialogOpen] = useState(false);
  const [executeError, setExecuteError] = useState<string | null>(null);
  const [executeLoading, setExecuteLoading] = useState(false);
  const { showSnackbar } = useSnackbar();

  const ownConfirmation = action.confirmations.find(
    (c) => c.confirming_party === party.memberPartyId,
  );

  const post = async <T,>(
    endpoint: string,
    body: T,
    successMsg: string,
  ): Promise<boolean> => {
    setBusy(true);
    try {
      const res = await authenticatedFetch(`${API_BASE}/governance/${endpoint}`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || `Failed: ${endpoint}`);
      }
      showSnackbar(successMsg);
      onAfter();
      return true;
    } catch (err) {
      showSnackbar(
        err instanceof Error ? err.message : `Failed: ${endpoint}`,
        "error",
      );
      return false;
    } finally {
      setBusy(false);
    }
  };

  const handleConfirm = async () => {
    if (!party.rulesContractId) {
      showSnackbar("Governance rules contract is not set", "error");
      return;
    }
    const body: ConfirmActionRequest = {
      party_id: party.partyId,
      rules_contract_id: party.rulesContractId,
      action: action.action,
      governance_type: party.governanceType,
    };
    await post("confirm", body, "Confirmation submitted");
  };

  const handleRevoke = async () => {
    if (!ownConfirmation) return;
    const body: CancelConfirmationRequest = {
      party_id: party.partyId,
      confirmation_cid: ownConfirmation.contract_id,
      governance_type: party.governanceType,
    };
    await post("cancel", body, "Confirmation revoked");
  };

  const handleExpire = async (confirmationCid: string) => {
    if (!party.rulesContractId) {
      showSnackbar("Governance rules contract is not set", "error");
      return;
    }
    const body: ExpireConfirmationRequest = {
      party_id: party.partyId,
      rules_contract_id: party.rulesContractId,
      confirmation_cid: confirmationCid,
      governance_type: party.governanceType,
    };
    await post("expire", body, "Confirmation expired");
  };

  const handleExecute = async (
    disclosedContracts: DisclosedContractInput[],
  ) => {
    if (!party.rulesContractId) {
      setExecuteError("Governance rules contract is not set");
      return;
    }
    setExecuteLoading(true);
    setExecuteError(null);
    try {
      const body: ExecuteActionRequest = {
        party_id: party.partyId,
        rules_contract_id: party.rulesContractId,
        action: action.action,
        confirmation_cids: action.confirmations.map((c) => c.contract_id),
        disclosed_contracts: disclosedContracts,
        governance_type: party.governanceType,
      };
      const res = await authenticatedFetch(`${API_BASE}/governance/execute`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || "Failed to execute");
      }
      showSnackbar("Action executed");
      setExecuteDialogOpen(false);
      onAfter();
    } catch (err) {
      setExecuteError(err instanceof Error ? err.message : "Failed to execute");
    } finally {
      setExecuteLoading(false);
    }
  };

  const details = formatActionDetails(action.action, party.threshold);
  const sorted = [...action.confirmations].sort(
    (a, b) => (a.created_at ?? 0) - (b.created_at ?? 0),
  );
  const proposerCid = sorted[0]?.contract_id;
  const nowSeconds = Math.floor(Date.now() / 1000);
  const needsYou = action.can_execute || !ownConfirmation;

  return (
    <>
      <ApprovalCard
        glyph="⬡"
        eyebrow="Governance"
        accent={needsYou}
        pill={
          <Pill
            label={
              action.can_execute
                ? "Ready to execute"
                : ownConfirmation
                  ? "Awaiting others"
                  : "Your vote needed"
            }
            tone={needsYou ? "accent" : "neutral"}
          />
        }
        time={
          action.last_confirmation_at
            ? formatRelativeTime(action.last_confirmation_at)
            : undefined
        }
        title={formatActionType(action.action)}
        facts={
          <Box
            sx={{
              display: "flex",
              alignItems: "center",
              gap: 0.5,
              fontSize: 13,
              color: "text.secondary",
            }}
          >
            on
            <Box
              component="span"
              onClick={() => onSelectParty(party.partyId)}
              sx={{
                fontFamily: "var(--font-mono)",
                fontSize: 12,
                color: "text.primary",
                bgcolor: "action.hover",
                borderRadius: "6px",
                px: 0.75,
                py: 0.25,
                cursor: "pointer",
                "&:hover": { color: "primary.main" },
              }}
            >
              {truncatePartyId(party.partyId)}
            </Box>
          </Box>
        }
        footerLeft={
          <ConfirmRing
            count={action.confirmation_count}
            threshold={party.threshold}
            canExecute={action.can_execute}
          />
        }
        actions={
          <>
            {ownConfirmation ? (
              <Button
                size="small"
                variant="outlined"
                color="warning"
                onClick={handleRevoke}
                disabled={busy}
              >
                Revoke
              </Button>
            ) : (
              <Button
                size="small"
                variant="contained"
                onClick={handleConfirm}
                disabled={busy || !party.rulesContractId}
              >
                Confirm
              </Button>
            )}
            {action.can_execute && (
              <Button
                size="small"
                variant="contained"
                color="success"
                onClick={() => {
                  setExecuteError(null);
                  setExecuteDialogOpen(true);
                }}
                disabled={busy || !party.rulesContractId}
              >
                Execute
              </Button>
            )}
          </>
        }
        detail={
          details.length > 0 || action.confirmations.length > 0 ? (
            <>
              {details.length > 0 && (
                <Box
                  sx={{
                    display: "flex",
                    flexDirection: "column",
                    gap: 0.5,
                    mb: action.confirmations.length > 0 ? 2 : 0,
                  }}
                >
                  {details.map((d, i) => (
                    <Box
                      key={i}
                      sx={{ display: "flex", alignItems: "baseline", gap: 1 }}
                    >
                      <Typography
                        variant="caption"
                        color="text.secondary"
                        sx={{ minWidth: 96 }}
                      >
                        {d.label}
                      </Typography>
                      {d.before !== undefined ? (
                        <Typography variant="body2">
                          <Box
                            component="span"
                            sx={{
                              color: "text.disabled",
                              textDecoration: "line-through",
                            }}
                          >
                            {d.before}
                          </Box>{" "}
                          →{" "}
                          <Box component="span" sx={{ fontWeight: 600 }}>
                            {d.after}
                          </Box>
                        </Typography>
                      ) : (
                        <Typography variant="body2" sx={{ fontWeight: 600 }}>
                          {d.after}
                        </Typography>
                      )}
                    </Box>
                  ))}
                </Box>
              )}
              {action.confirmations.length > 0 && (
                <Typography
                  sx={{
                    fontFamily: "var(--font-mono)",
                    fontSize: 10,
                    letterSpacing: "0.12em",
                    textTransform: "uppercase",
                    color: "text.secondary",
                    mb: 1,
                  }}
                >
                  Confirmations · {action.confirmation_count} of {party.threshold}
                </Typography>
              )}
              <Box sx={{ display: "flex", flexDirection: "column", gap: 0.5 }}>
                {sorted.map((c) => {
                  const isOwn = c.confirming_party === party.memberPartyId;
                  const isProposer = c.contract_id === proposerCid;
                  const isExpired =
                    (c.expires_at ?? 0) > 0 &&
                    (c.expires_at ?? 0) <= nowSeconds;
                  return (
                    <Box
                      key={c.contract_id}
                      sx={{ display: "flex", alignItems: "center", gap: 0.75 }}
                    >
                      <Typography
                        variant="caption"
                        sx={{
                          fontFamily: "var(--font-mono)",
                          color: isExpired
                            ? "text.disabled"
                            : isOwn
                              ? "primary.main"
                              : "text.primary",
                          textDecoration: isExpired ? "line-through" : "none",
                        }}
                      >
                        {truncatePartyId(c.confirming_party)}
                        {isOwn ? " (you)" : ""}
                      </Typography>
                      {isProposer && (
                        <Chip
                          label="proposer"
                          size="small"
                          variant="outlined"
                          sx={{
                            height: 18,
                            "& .MuiChip-label": {
                              px: 0.75,
                              fontSize: 10,
                              lineHeight: 1,
                            },
                          }}
                        />
                      )}
                      {isExpired && (
                        <Chip
                          label="expired"
                          size="small"
                          variant="outlined"
                          color="warning"
                          sx={{
                            height: 18,
                            "& .MuiChip-label": {
                              px: 0.75,
                              fontSize: 10,
                              lineHeight: 1,
                            },
                          }}
                        />
                      )}
                      {!isOwn && !isProposer && isExpired && (
                        <Tooltip title="Expire confirmation">
                          <span>
                            <IconButton
                              size="small"
                              onClick={() => handleExpire(c.contract_id)}
                              disabled={busy || !party.rulesContractId}
                              sx={{ p: 0.25 }}
                            >
                              <TimerOffIcon sx={{ fontSize: 14 }} />
                            </IconButton>
                          </span>
                        </Tooltip>
                      )}
                    </Box>
                  );
                })}
              </Box>
            </>
          ) : undefined
        }
      />
      <ExecuteDialog
        open={executeDialogOpen}
        onClose={() => setExecuteDialogOpen(false)}
        onExecute={handleExecute}
        action={action}
        loading={executeLoading}
        error={executeError}
        onErrorDismiss={() => setExecuteError(null)}
      />
    </>
  );
};

const DomainActionCard = ({
  party,
  domainAction,
  onAfter,
  onSelectParty,
}: {
  party: PartyActions;
  domainAction: DomainGovernanceAction;
  onAfter: () => void;
  onSelectParty: (partyId: string) => void;
}) => {
  const [busy, setBusy] = useState(false);
  const { showSnackbar } = useSnackbar();

  const ownConfirmation = domainAction.confirmations.find(
    (c) => c.confirming_party === party.memberPartyId,
  );

  // The on-chain proposal already encodes the action — server only needs the
  // proposal_cid and governance_type to confirm/execute. action is a
  // placeholder kept for payload symmetry with the off-chain path.
  const placeholderAction = {
    type: "governance_set_threshold" as const,
    new_threshold: 0,
  };

  const post = async <T,>(
    endpoint: string,
    body: T,
    successMsg: string,
  ): Promise<void> => {
    if (!party.rulesContractId) {
      showSnackbar("Governance rules contract is not set", "error");
      return;
    }
    setBusy(true);
    try {
      const res = await authenticatedFetch(`${API_BASE}/governance/${endpoint}`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || `Failed: ${endpoint}`);
      }
      showSnackbar(successMsg);
      onAfter();
    } catch (err) {
      showSnackbar(
        err instanceof Error ? err.message : `Failed: ${endpoint}`,
        "error",
      );
    } finally {
      setBusy(false);
    }
  };

  const handleConfirm = () =>
    post(
      "confirm",
      {
        party_id: party.partyId,
        rules_contract_id: party.rulesContractId,
        action: placeholderAction,
        governance_type: "core_domain" as const,
        proposal_cid: domainAction.proposal_cid,
      },
      "Confirmation submitted",
    );

  const handleRevoke = () => {
    if (!ownConfirmation) return;
    return post(
      "cancel",
      {
        party_id: party.partyId,
        confirmation_cid: ownConfirmation.contract_id,
        governance_type: "core_domain" as const,
      },
      "Confirmation revoked",
    );
  };

  const handleExpire = (confirmationCid: string) =>
    post(
      "expire",
      {
        party_id: party.partyId,
        rules_contract_id: party.rulesContractId,
        confirmation_cid: confirmationCid,
        governance_type: "core_domain" as const,
      },
      "Confirmation expired",
    );

  // For orphaned actions (underlying proposal is gone) the only valid
  // operation is to clear the stranded Confirmation contracts off the ledger.
  // Loop sequentially so the `busy` lock on each /governance/expire call
  // serializes correctly.
  const handleDismissOrphan = async () => {
    for (const c of domainAction.confirmations) {
      await handleExpire(c.contract_id);
    }
  };

  const handleExecute = () =>
    post(
      "execute",
      {
        party_id: party.partyId,
        rules_contract_id: party.rulesContractId,
        action: placeholderAction,
        confirmation_cids: domainAction.confirmations.map((c) => c.contract_id),
        disclosed_contracts: [],
        governance_type: "core_domain" as const,
        proposal_cid: domainAction.proposal_cid,
      },
      "Proposal executed",
    );

  const isTransfer =
    !!domainAction.transfer_details || !!domainAction.accept_transfer_details;
  const latestConfirm = domainAction.confirmations.reduce(
    (max, c) => Math.max(max, c.created_at ?? 0),
    0,
  );
  const needsYou =
    domainAction.can_execute || !!domainAction.orphaned || !ownConfirmation;

  return (
    <ApprovalCard
      glyph={isTransfer ? "₿" : "⬡"}
      eyebrow={isTransfer ? "Transfer" : "Governance"}
      accent={needsYou}
      pill={
        <Pill
          label={
            domainAction.orphaned
              ? "Orphaned"
              : domainAction.can_execute
                ? "Ready to execute"
                : ownConfirmation
                  ? "Awaiting others"
                  : "Your vote needed"
          }
          tone={
            domainAction.orphaned
              ? "danger"
              : domainAction.can_execute || !ownConfirmation
                ? "accent"
                : "neutral"
          }
        />
      }
      time={latestConfirm > 0 ? formatRelativeTime(latestConfirm) : undefined}
      title={domainAction.action_label}
      facts={
        <Box
          sx={{
            display: "flex",
            alignItems: "center",
            gap: 0.5,
            fontSize: 13,
            color: "text.secondary",
          }}
        >
          on
          <Box
            component="span"
            onClick={() => onSelectParty(party.partyId)}
            sx={{
              fontFamily: "var(--font-mono)",
              fontSize: 12,
              color: "text.primary",
              bgcolor: "action.hover",
              borderRadius: "6px",
              px: 0.75,
              py: 0.25,
              cursor: "pointer",
              "&:hover": { color: "primary.main" },
            }}
          >
            {truncatePartyId(party.partyId)}
          </Box>
        </Box>
      }
      footerLeft={
        <ConfirmRing
          count={domainAction.confirmation_count}
          threshold={party.threshold}
          canExecute={domainAction.can_execute}
        />
      }
      actions={
        domainAction.orphaned ? (
          <Button
            size="small"
            variant="outlined"
            color="warning"
            onClick={handleDismissOrphan}
            disabled={busy || !party.rulesContractId}
          >
            Dismiss
          </Button>
        ) : (
          <>
            {ownConfirmation ? (
              <Button
                size="small"
                variant="outlined"
                color="warning"
                onClick={handleRevoke}
                disabled={busy}
              >
                Revoke
              </Button>
            ) : (
              <Button
                size="small"
                variant="contained"
                onClick={handleConfirm}
                disabled={busy || !party.rulesContractId}
              >
                Confirm
              </Button>
            )}
            {domainAction.can_execute && (
              <Button
                size="small"
                variant="contained"
                color="success"
                onClick={handleExecute}
                disabled={busy || !party.rulesContractId}
              >
                Execute
              </Button>
            )}
          </>
        )
      }
      detail={
        <Box sx={{ display: "flex", flexDirection: "column", gap: 1 }}>

      {domainAction.orphaned && (
        <Alert severity="warning" sx={{ py: 0.5 }}>
          The underlying proposal has been archived. These confirmation
          contracts are stranded on the ledger — dismiss to clear them.
        </Alert>
      )}

      {domainAction.description && (
        <Typography
          variant="body2"
          sx={{ px: 1.25, py: 1, bgcolor: "action.hover", borderRadius: 1 }}
        >
          {domainAction.description}
        </Typography>
      )}

      {domainAction.transfer_details && (() => {
        const td = domainAction.transfer_details;
        // Canton Coin's token-standard `instrument_id` is the literal
        // "Amulet" — render as "CC" to match Holdings and the Transfer
        // Proposal dropdown.
        const token =
          td.instrument_id === "Amulet" ? "CC" : td.instrument_id;
        const rows: { label: string; value: string; copyValue?: string }[] = [
          { label: "Token", value: token },
          { label: "Amount", value: td.amount },
          {
            label: "Recipient",
            value: truncatePartyId(td.receiver),
            copyValue: td.receiver,
          },
        ];
        return (
          <Box
            sx={{
              display: "flex",
              flexDirection: "column",
              gap: 0.75,
              px: 1.25,
              py: 1,
              bgcolor: "action.hover",
              borderRadius: 1,
            }}
          >
            {rows.map((r) => (
              <Box
                key={r.label}
                sx={{ display: "flex", alignItems: "center", gap: 1 }}
              >
                <Typography
                  variant="caption"
                  color="text.secondary"
                  sx={{ minWidth: 96 }}
                >
                  {r.label}
                </Typography>
                <Typography variant="body2" sx={{ fontWeight: 600 }}>
                  {r.value}
                </Typography>
                {r.copyValue && (
                  <Tooltip title={`Copy ${r.label.toLowerCase()}`}>
                    <IconButton
                      size="small"
                      onClick={async () => {
                        const ok = await copyToClipboard(r.copyValue!);
                        showSnackbar(
                          ok ? "Copied to clipboard" : "Failed to copy",
                        );
                      }}
                      sx={{ p: 0.25 }}
                    >
                      <ContentCopyIcon sx={{ fontSize: 14 }} />
                    </IconButton>
                  </Tooltip>
                )}
              </Box>
            ))}
          </Box>
        );
      })()}

      {domainAction.accept_transfer_details && (() => {
        const atd = domainAction.accept_transfer_details;
        // Same Amulet → CC rename as the Transfer card for consistency.
        const token =
          atd.instrument_id === "Amulet" ? "CC" : atd.instrument_id;
        const rows: { label: string; value: string; copyValue?: string }[] = [
          { label: "Token", value: token },
          { label: "Amount", value: atd.amount },
          {
            label: "Sender",
            value: truncatePartyId(atd.sender),
            copyValue: atd.sender,
          },
          {
            label: "Recipient",
            value: truncatePartyId(atd.receiver),
            copyValue: atd.receiver,
          },
        ];
        return (
          <Box
            sx={{
              display: "flex",
              flexDirection: "column",
              gap: 0.75,
              px: 1.25,
              py: 1,
              bgcolor: "action.hover",
              borderRadius: 1,
            }}
          >
            {rows.map((r) => (
              <Box
                key={r.label}
                sx={{ display: "flex", alignItems: "center", gap: 1 }}
              >
                <Typography
                  variant="caption"
                  color="text.secondary"
                  sx={{ minWidth: 96 }}
                >
                  {r.label}
                </Typography>
                <Typography variant="body2" sx={{ fontWeight: 600 }}>
                  {r.value}
                </Typography>
                {r.copyValue && (
                  <Tooltip title={`Copy ${r.label.toLowerCase()}`}>
                    <IconButton
                      size="small"
                      onClick={async () => {
                        const ok = await copyToClipboard(r.copyValue!);
                        showSnackbar(
                          ok ? "Copied to clipboard" : "Failed to copy",
                        );
                      }}
                      sx={{ p: 0.25 }}
                    >
                      <ContentCopyIcon sx={{ fontSize: 14 }} />
                    </IconButton>
                  </Tooltip>
                )}
              </Box>
            ))}
          </Box>
        );
      })()}

      {domainAction.service_request_details && (() => {
        const srd = domainAction.service_request_details;
        // The proposal type itself is the action_label header above; here we
        // surface the operator and the user/provider being onboarded.
        const rows: { label: string; value: string; copyValue?: string }[] = [
          {
            label: "Operator",
            value: truncatePartyId(srd.operator),
            copyValue: srd.operator,
          },
        ];
        if (srd.user) {
          rows.push({
            label: "User",
            value: truncatePartyId(srd.user),
            copyValue: srd.user,
          });
        }
        if (srd.provider) {
          rows.push({
            label: "Provider",
            value: truncatePartyId(srd.provider),
            copyValue: srd.provider,
          });
        }
        return (
          <Box
            sx={{
              display: "flex",
              flexDirection: "column",
              gap: 0.75,
              px: 1.25,
              py: 1,
              bgcolor: "action.hover",
              borderRadius: 1,
            }}
          >
            {rows.map((r) => (
              <Box
                key={r.label}
                sx={{ display: "flex", alignItems: "center", gap: 1 }}
              >
                <Typography
                  variant="caption"
                  color="text.secondary"
                  sx={{ minWidth: 96 }}
                >
                  {r.label}
                </Typography>
                <Typography variant="body2" sx={{ fontWeight: 600 }}>
                  {r.value}
                </Typography>
                {r.copyValue && (
                  <Tooltip title={`Copy ${r.label.toLowerCase()}`}>
                    <IconButton
                      size="small"
                      onClick={async () => {
                        const ok = await copyToClipboard(r.copyValue!);
                        showSnackbar(
                          ok ? "Copied to clipboard" : "Failed to copy",
                        );
                      }}
                      sx={{ p: 0.25 }}
                    >
                      <ContentCopyIcon sx={{ fontSize: 14 }} />
                    </IconButton>
                  </Tooltip>
                )}
              </Box>
            ))}
          </Box>
        );
      })()}

      <Box sx={{ display: "flex", alignItems: "center", gap: 0.5 }}>
        <Typography
          variant="caption"
          color="text.secondary"
          sx={{ fontFamily: "var(--font-mono)" }}
        >
          {domainAction.proposal_cid.slice(0, 16)}…
        </Typography>
        <Tooltip title="Copy proposal contract id">
          <IconButton
            size="small"
            onClick={async () => {
              const ok = await copyToClipboard(domainAction.proposal_cid);
              showSnackbar(ok ? "Copied to clipboard" : "Failed to copy");
            }}
            sx={{ p: 0.25 }}
          >
            <ContentCopyIcon sx={{ fontSize: 14 }} />
          </IconButton>
        </Tooltip>
      </Box>

      {domainAction.confirmations.length > 0 && (() => {
        const sorted = [...domainAction.confirmations].sort(
          (a, b) => (a.created_at ?? 0) - (b.created_at ?? 0),
        );
        const proposerCid = sorted[0]?.contract_id;
        const nowSeconds = Math.floor(Date.now() / 1000);
        return (
          <Box
            sx={{
              display: "flex",
              alignItems: "baseline",
              gap: 1,
              px: 1.25,
              py: 1,
              bgcolor: "action.hover",
              borderRadius: 1,
            }}
          >
            <Typography
              variant="caption"
              color="text.secondary"
              sx={{ minWidth: 96 }}
            >
              Confirmed by
            </Typography>
            <Box sx={{ display: "flex", flexDirection: "column", gap: 0.25 }}>
              {sorted.map((c) => {
                const isOwn = c.confirming_party === party.memberPartyId;
                const isProposer = c.contract_id === proposerCid;
                const isExpired =
                  (c.expires_at ?? 0) > 0 && (c.expires_at ?? 0) <= nowSeconds;
                return (
                  <Box
                    key={c.contract_id}
                    sx={{ display: "flex", alignItems: "center", gap: 0.5 }}
                  >
                    <Typography
                      variant="caption"
                      sx={{
                        fontFamily: "var(--font-mono)",
                        color: isExpired
                          ? "text.disabled"
                          : isOwn
                            ? "primary.main"
                            : "text.primary",
                        textDecoration: isExpired ? "line-through" : "none",
                      }}
                    >
                      {truncatePartyId(c.confirming_party)}
                      {isOwn ? " (you)" : ""}
                    </Typography>
                    {isProposer && (
                      <Chip
                        label="proposer"
                        size="small"
                        variant="outlined"
                        sx={{
                          height: 18,
                          "& .MuiChip-label": {
                            px: 0.75,
                            fontSize: 10,
                            lineHeight: 1,
                          },
                        }}
                      />
                    )}
                    {isExpired && (
                      <Chip
                        label="expired"
                        size="small"
                        variant="outlined"
                        color="warning"
                        sx={{
                          height: 18,
                          "& .MuiChip-label": {
                            px: 0.75,
                            fontSize: 10,
                            lineHeight: 1,
                          },
                        }}
                      />
                    )}
                    {!isOwn && !isProposer && isExpired && (
                      <Tooltip title="Expire confirmation">
                        <span>
                          <IconButton
                            size="small"
                            onClick={() => handleExpire(c.contract_id)}
                            disabled={busy || !party.rulesContractId}
                            sx={{ p: 0.25 }}
                          >
                            <TimerOffIcon sx={{ fontSize: 14 }} />
                          </IconButton>
                        </span>
                      </Tooltip>
                    )}
                  </Box>
                );
              })}
            </Box>
          </Box>
        );
      })()}

        </Box>
      }
    />
  );
};

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

  const cancelEndpointForKind = () => {
    switch (run.kind) {
      case "Onboarding":
        return `${API_BASE}/onboarding/cancel`;
      case "Kick":
        return `${API_BASE}/kick/cancel`;
      case "Contracts":
        return `${API_BASE}/contracts/cancel`;
      case "Dars":
        return `${API_BASE}/dars/cancel`;
    }
  };

  const cancel = async () => {
    setBusy(true);
    try {
      const res = await authenticatedFetch(cancelEndpointForKind(), {
        method: "POST",
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || "Failed to cancel");
      }
      showSnackbar(`${run.kind} workflow cancelled`);
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
      showSnackbar(`Retrying ${run.kind} workflow`);
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
      <Box
        sx={{
          display: "flex",
          alignItems: "center",
          gap: 1.5,
          p: "9px 14px",
          border: "1px solid",
          borderColor: "divider",
          borderRadius: "8px",
          bgcolor: "background.paper",
        }}
      >
        <Box
          component="span"
          sx={{
            color: "text.disabled",
            fontFamily: "var(--font-mono)",
            fontSize: 13,
            flexShrink: 0,
          }}
        >
          ◆
        </Box>
        <Typography sx={{ fontSize: 14, fontWeight: 500, whiteSpace: "nowrap" }}>
          {run.kind} workflow
        </Typography>
        <Typography
          sx={{
            fontFamily: "var(--font-mono)",
            fontSize: 12,
            color: "text.secondary",
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
            minWidth: 0,
          }}
        >
          {run.prefix ||
            (run.dec_party_id ? truncatePartyId(run.dec_party_id) : fromLine || "")}
        </Typography>
        <Box sx={{ flex: 1, minWidth: 8 }} />
        <StatusPill status={run.status} />
        <Typography
          sx={{
            fontFamily: "var(--font-mono)",
            fontSize: 12,
            color: "text.secondary",
            whiteSpace: "nowrap",
          }}
        >
          {formatRelativeTime(run.updated_at)}
        </Typography>
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
    );
  }

  return (
    <ApprovalCard
      glyph="◆"
      eyebrow={`Workflow · ${run.kind}`}
      pill={<StatusPill status={run.status} />}
      time={formatRelativeTime(run.updated_at)}
      title={
        run.prefix ? `${run.kind} workflow · ${run.prefix}` : `${run.kind} workflow`
      }
      facts={
        <>
          {fromLine && (
            <Typography sx={{ fontSize: 13, color: "text.secondary" }}>
              {fromLine}
            </Typography>
          )}
          {isInProgress && run.step_total > 0 && (
            <Box sx={{ mt: fromLine ? 1.25 : 0 }}>
              <WorkflowPipeline
                current={run.step_index}
                total={run.step_total}
                label={run.current_step}
              />
            </Box>
          )}
        </>
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
                      sx={{ color: "text.secondary", textDecoration: "line-through" }}
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

export const NotificationsView = ({
  pendingInvitations,
  partyActions,
  workflowRuns,
  loading,
  onInvitationsChanged,
  onActionsChanged,
  onWorkflowsChanged,
  onSelectParty,
}: NotificationsViewProps) => {
  const [statusFilter, setStatusFilter] = useState<
    "all" | "you" | "prog" | "done"
  >("all");
  const [typeFilter, setTypeFilter] = useState<"all" | "gov" | "wf" | "inv">(
    "all",
  );
  const [doneCollapsed, setDoneCollapsed] = useState(true);

  if (loading) {
    return (
      <Box sx={{ display: "flex", flexDirection: "column", gap: 1, p: 3 }}>
        <NotificationSkeleton />
        <NotificationSkeleton />
        <NotificationSkeleton />
      </Box>
    );
  }

  // ── Categorize every feed item by what it needs from the operator ──
  type Group = "you" | "prog" | "done";
  type Kind = "gov" | "wf" | "inv";
  interface Entry {
    key: string;
    group: Group;
    kind: Kind;
    ts: number;
    node: ReactNode;
  }

  const entries: Entry[] = [];

  for (const invitation of pendingInvitations) {
    entries.push({
      key: `inv-${invitation.id}`,
      group: "you",
      kind: "inv",
      ts: invitation.received_at,
      node: (
        <InvitationCard
          invitation={invitation}
          onAfter={onInvitationsChanged}
          onSelectParty={onSelectParty}
        />
      ),
    });
  }

  for (const party of partyActions) {
    for (const action of party.actions) {
      const ownConfirmed = action.confirmations.some(
        (c) => c.confirming_party === party.memberPartyId,
      );
      entries.push({
        key: `act-${party.partyId}-${action.action_hash}`,
        // Needs you: you can execute it, or you haven't confirmed yet.
        // In progress: you confirmed and it's waiting on the others.
        group: action.can_execute || !ownConfirmed ? "you" : "prog",
        kind: "gov",
        ts: action.last_confirmation_at ?? 0,
        node: (
          <ActionCard
            party={party}
            action={action}
            onAfter={onActionsChanged}
            onSelectParty={onSelectParty}
          />
        ),
      });
    }
    for (const domainAction of party.domainActions) {
      const ownConfirmed = domainAction.confirmations.some(
        (c) => c.confirming_party === party.memberPartyId,
      );
      entries.push({
        key: `dom-${party.partyId}-${domainAction.proposal_cid}`,
        // Orphaned proposals can only be expired — still your cleanup to do.
        group:
          domainAction.can_execute || domainAction.orphaned || !ownConfirmed
            ? "you"
            : "prog",
        kind: "gov",
        ts: domainAction.confirmations.reduce(
          (max, c) => Math.max(max, c.created_at ?? 0),
          0,
        ),
        node: (
          <DomainActionCard
            party={party}
            domainAction={domainAction}
            onAfter={onActionsChanged}
            onSelectParty={onSelectParty}
          />
        ),
      });
    }
  }

  for (const run of workflowRuns) {
    const terminal =
      run.status === "completed" ||
      run.status === "failed" ||
      run.status === "cancelled";
    entries.push({
      key: `wf-${run.instance_name}`,
      group: terminal ? "done" : "prog",
      kind: "wf",
      ts: run.updated_at,
      node: (
        <WorkflowRunCard
          run={run}
          onAfter={onWorkflowsChanged}
          onSelectParty={onSelectParty}
          compact={terminal}
        />
      ),
    });
  }

  entries.sort((a, b) => b.ts - a.ts);

  if (entries.length === 0) {
    return (
      <Box sx={{ p: 4, textAlign: "center" }}>
        <Typography variant="body2" color="text.secondary">
          No pending notifications.
        </Typography>
      </Box>
    );
  }

  // Each chip's count reflects the *other* active filter dimension.
  const byKind = entries.filter(
    (e) => typeFilter === "all" || e.kind === typeFilter,
  );
  const groupCount: Record<"all" | Group, number> = {
    all: byKind.length,
    you: byKind.filter((e) => e.group === "you").length,
    prog: byKind.filter((e) => e.group === "prog").length,
    done: byKind.filter((e) => e.group === "done").length,
  };

  const grouped: Record<Group, Entry[]> = { you: [], prog: [], done: [] };
  for (const e of entries) {
    if (statusFilter !== "all" && statusFilter !== e.group) continue;
    if (typeFilter !== "all" && typeFilter !== e.kind) continue;
    grouped[e.group].push(e);
  }
  const anyVisible =
    grouped.you.length + grouped.prog.length + grouped.done.length > 0;

  const statusChips: { key: "all" | Group; label: string }[] = [
    { key: "all", label: "All" },
    { key: "you", label: "Needs you" },
    { key: "prog", label: "In progress" },
    { key: "done", label: "Completed" },
  ];
  const typeChips: { key: "all" | Kind; label: string }[] = [
    { key: "all", label: "All" },
    { key: "gov", label: "Governance" },
    { key: "wf", label: "Workflows" },
    { key: "inv", label: "Invitations" },
  ];
  const sections: { group: Group; label: string }[] = [
    { group: "you", label: "Needs your action" },
    { group: "prog", label: "In progress" },
    { group: "done", label: "Completed" },
  ];

  return (
    <Box sx={{ py: 3, px: "var(--content-pad)" }}>
      <Box
        sx={{
          position: "sticky",
          top: 0,
          zIndex: 5,
          display: "flex",
          flexWrap: "wrap",
          alignItems: "center",
          gap: 1,
          py: 1.5,
          mb: 1,
          bgcolor: "background.default",
          borderBottom: "1px solid",
          borderColor: "divider",
        }}
      >
        <Box sx={{ display: "flex", flexWrap: "wrap", gap: 1 }}>
          {statusChips.map((c) => {
            const active = statusFilter === c.key;
            return (
              <Box
                component="button"
                key={c.key}
                onClick={() => setStatusFilter(c.key)}
                sx={{
                  display: "inline-flex",
                  alignItems: "center",
                  gap: 0.75,
                  fontFamily: "inherit",
                  fontSize: 13,
                  fontWeight: 500,
                  color: active ? "text.primary" : "text.secondary",
                  bgcolor: active
                    ? "rgba(214, 58, 15, 0.08)"
                    : "background.paper",
                  border: "1px solid",
                  borderColor: active ? "var(--accent)" : "divider",
                  borderRadius: "8px",
                  px: 1.5,
                  py: 0.75,
                  cursor: "pointer",
                  transition: "color .15s, border-color .15s, background .15s",
                  "&:hover": { color: "text.primary" },
                }}
              >
                {c.label}
                <Box
                  component="span"
                  sx={{
                    fontFamily: "var(--font-mono)",
                    fontSize: 12,
                    fontWeight: 600,
                    color:
                      c.key === "you"
                        ? "var(--accent)"
                        : active
                          ? "text.primary"
                          : "text.disabled",
                  }}
                >
                  {groupCount[c.key]}
                </Box>
              </Box>
            );
          })}
        </Box>
        <Box sx={{ flex: 1, minWidth: 8 }} />
        <Box sx={{ display: "flex", gap: 0.75 }}>
          {typeChips.map((c) => {
            const active = typeFilter === c.key;
            return (
              <Box
                component="button"
                key={c.key}
                onClick={() => setTypeFilter(c.key)}
                sx={{
                  fontFamily: "var(--font-mono)",
                  fontSize: 12,
                  letterSpacing: "0.04em",
                  textTransform: "uppercase",
                  color: active ? "text.primary" : "text.disabled",
                  bgcolor: active ? "background.paper" : "transparent",
                  border: "1px solid",
                  borderColor: active ? "text.disabled" : "divider",
                  borderRadius: "6px",
                  px: 1.1,
                  py: 0.65,
                  cursor: "pointer",
                  transition: "color .15s, border-color .15s",
                }}
              >
                {c.label}
              </Box>
            );
          })}
        </Box>
      </Box>

      {!anyVisible && (
        <Typography
          variant="body2"
          color="text.secondary"
          sx={{ textAlign: "center", py: 6 }}
        >
          Nothing matches these filters.
        </Typography>
      )}

      {sections.map(({ group, label }) => {
        if (statusFilter !== "all" && statusFilter !== group) return null;
        if (grouped[group].length === 0) return null;
        const collapsible = group === "done";
        const collapsed =
          collapsible && doneCollapsed && statusFilter !== "done";
        return (
          <Box key={group} sx={{ mt: 3 }}>
            <Box
              onClick={
                collapsible ? () => setDoneCollapsed((v) => !v) : undefined
              }
              sx={{
                display: "flex",
                alignItems: "center",
                gap: 1.25,
                mb: 1.5,
                cursor: collapsible ? "pointer" : "default",
              }}
            >
              {collapsible && (
                <Box
                  component="span"
                  sx={{
                    display: "flex",
                    alignItems: "center",
                    fontFamily: "var(--font-mono)",
                    fontSize: 11,
                    lineHeight: 1,
                    color: "text.disabled",
                  }}
                >
                  {collapsed ? "▸" : "▾"}
                </Box>
              )}
              <Typography
                component="h2"
                sx={{
                  fontFamily: "var(--font-mono)",
                  fontSize: 11,
                  fontWeight: 500,
                  letterSpacing: "0.12em",
                  textTransform: "uppercase",
                  m: 0,
                  lineHeight: 1,
                  color: group === "you" ? "var(--accent)" : "text.secondary",
                }}
              >
                {label}
              </Typography>
              <Box
                component="span"
                sx={{
                  fontFamily: "var(--font-mono)",
                  fontSize: 11,
                  fontWeight: 600,
                  color: "text.secondary",
                  bgcolor: "action.hover",
                  borderRadius: "20px",
                  px: 1,
                  py: 0.25,
                }}
              >
                {groupCount[group]}
              </Box>
              <Box sx={{ flex: 1, height: "1px", bgcolor: "divider" }} />
            </Box>
            {!collapsed && (
              <Box sx={{ display: "flex", flexDirection: "column", gap: 1 }}>
                {grouped[group].map((e) => (
                  <Fragment key={e.key}>{e.node}</Fragment>
                ))}
              </Box>
            )}
          </Box>
        );
      })}
    </Box>
  );
};
