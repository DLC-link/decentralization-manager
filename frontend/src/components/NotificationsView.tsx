import { useState } from "react";
import {
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
}: {
  invitation: PendingInvitation;
  onAfter: () => void;
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
      showSnackbar(err instanceof Error ? err.message : `Failed to ${path}`);
    } finally {
      setBusy(false);
    }
  };

  const fromLabel =
    invitation.coordinator_name ||
    `${invitation.coordinator_pubkey.slice(0, 12)}…${invitation.coordinator_pubkey.slice(-6)}`;
  const showOnboardingMeta =
    invitation.invitation_type === "Onboarding" &&
    (!!invitation.prefix || (invitation.participants?.length ?? 0) > 0);
  const showDarsMeta =
    invitation.invitation_type === "Dars" &&
    (invitation.dar_filenames?.length ?? 0) > 0;

  return (
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
        <Box>
          <Typography variant="subtitle2" sx={{ fontWeight: 600 }}>
            {invitation.invitation_type} invitation
          </Typography>
          <Box sx={{ display: "flex", alignItems: "center", gap: 0.25 }}>
            <Typography variant="caption" color="text.secondary">
              from{" "}
              {invitation.coordinator_name ? (
                fromLabel
              ) : (
                <Box component="span" sx={{ fontFamily: "monospace" }}>
                  {fromLabel}
                </Box>
              )}
            </Typography>
            <Tooltip title="Copy sender public key">
              <IconButton
                size="small"
                onClick={async () => {
                  const ok = await copyToClipboard(invitation.coordinator_pubkey);
                  showSnackbar(ok ? "Copied to clipboard" : "Failed to copy");
                }}
                sx={{ p: 0.25 }}
              >
                <ContentCopyIcon sx={{ fontSize: 14 }} />
              </IconButton>
            </Tooltip>
          </Box>
        </Box>
        <Typography variant="caption" color="text.secondary">
          {formatRelativeTime(invitation.received_at)}
        </Typography>
      </Box>

      {showOnboardingMeta && (
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
        </Box>
      )}

      {showDarsMeta && (
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
            DARs ({invitation.dar_filenames?.length})
          </Typography>
          <Box sx={{ display: "flex", flexWrap: "wrap", gap: 0.5 }}>
            {invitation.dar_filenames?.map((filename) => (
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

      <Box sx={{ display: "flex", gap: 1, justifyContent: "flex-end" }}>
        <Button
          variant="text"
          color="inherit"
          size="small"
          onClick={() => respond("decline")}
          disabled={busy}
        >
          Deny
        </Button>
        <Button
          variant="outlined"
          color="primary"
          size="small"
          onClick={() => respond("accept")}
          disabled={busy}
          startIcon={busy ? <CircularProgress size={14} /> : undefined}
        >
          Accept
        </Button>
      </Box>
    </Box>
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
      );
      return false;
    } finally {
      setBusy(false);
    }
  };

  const handleConfirm = async () => {
    if (!party.rulesContractId) {
      showSnackbar("Governance rules contract is not set");
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
      showSnackbar("Governance rules contract is not set");
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

  return (
    <Box
      sx={{
        p: 2,
        border: 1,
        borderColor: "divider",
        borderRadius: 2,
        display: "flex",
        flexDirection: "column",
        gap: 1,
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
        <Box sx={{ display: "flex", flexDirection: "column", gap: 0.25 }}>
          <Typography variant="subtitle2" sx={{ fontWeight: 600 }}>
            {formatActionType(action.action)}
          </Typography>
          <Box sx={{ display: "flex", alignItems: "baseline", gap: 0.5 }}>
            <Typography variant="caption" color="text.secondary">
              on
            </Typography>
            <Typography
              component="span"
              variant="caption"
              onClick={() => onSelectParty(party.partyId)}
              sx={{
                fontFamily: "monospace",
                color: "primary.main",
                cursor: "pointer",
                "&:hover": { textDecoration: "underline" },
              }}
            >
              {truncatePartyId(party.partyId)}
            </Typography>
          </Box>
        </Box>
        {action.last_confirmation_at ? (
          <Typography variant="caption" color="text.secondary">
            {formatRelativeTime(action.last_confirmation_at)}
          </Typography>
        ) : null}
      </Box>

      {(() => {
        const details = formatActionDetails(action.action, party.threshold);
        if (details.length === 0) return null;
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
                      sx={{ color: "text.secondary", textDecoration: "line-through" }}
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
        );
      })()}

      {action.confirmations.length > 0 && (() => {
        const sorted = [...action.confirmations].sort(
          (a, b) => (a.created_at ?? 0) - (b.created_at ?? 0),
        );
        const proposerCid = sorted[0]?.contract_id;
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
                return (
                  <Box
                    key={c.contract_id}
                    sx={{ display: "flex", alignItems: "center", gap: 0.5 }}
                  >
                    <Typography
                      variant="caption"
                      sx={{
                        fontFamily: "monospace",
                        color: isOwn ? "primary.main" : "text.primary",
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
                    {!isOwn && (
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

      <Box
        sx={{
          display: "flex",
          gap: 1,
          alignItems: "center",
          justifyContent: "flex-end",
        }}
      >
        <Chip
          label={`${action.confirmation_count} / ${party.threshold} confirmed`}
          size="small"
          color={action.can_execute ? "success" : "default"}
          variant={action.can_execute ? "filled" : "outlined"}
        />
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
            variant="outlined"
            onClick={handleConfirm}
            disabled={busy || !party.rulesContractId}
          >
            Confirm
          </Button>
        )}
        {action.can_execute && (
          <Button
            size="small"
            variant="outlined"
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
      </Box>

      <ExecuteDialog
        open={executeDialogOpen}
        onClose={() => setExecuteDialogOpen(false)}
        onExecute={handleExecute}
        action={action}
        loading={executeLoading}
        error={executeError}
      />
    </Box>
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
      showSnackbar("Governance rules contract is not set");
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
      showSnackbar(err instanceof Error ? err.message : `Failed: ${endpoint}`);
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

  return (
    <Box
      sx={{
        p: 2,
        border: 1,
        borderColor: "divider",
        borderRadius: 2,
        display: "flex",
        flexDirection: "column",
        gap: 1,
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
        <Box sx={{ display: "flex", flexDirection: "column", gap: 0.25 }}>
          <Typography variant="subtitle2" sx={{ fontWeight: 600 }}>
            {domainAction.action_label}
          </Typography>
          <Box sx={{ display: "flex", alignItems: "baseline", gap: 0.5 }}>
            <Typography variant="caption" color="text.secondary">
              on
            </Typography>
            <Typography
              component="span"
              variant="caption"
              onClick={() => onSelectParty(party.partyId)}
              sx={{
                fontFamily: "monospace",
                color: "primary.main",
                cursor: "pointer",
                "&:hover": { textDecoration: "underline" },
              }}
            >
              {truncatePartyId(party.partyId)}
            </Typography>
          </Box>
        </Box>
        <Chip label="proposal" size="small" variant="outlined" />
      </Box>

      {domainAction.description && (
        <Typography
          variant="body2"
          sx={{ px: 1.25, py: 1, bgcolor: "action.hover", borderRadius: 1 }}
        >
          {domainAction.description}
        </Typography>
      )}

      <Typography
        variant="caption"
        color="text.secondary"
        sx={{ fontFamily: "monospace" }}
      >
        {domainAction.proposal_cid.slice(0, 16)}…
      </Typography>

      {domainAction.confirmations.length > 0 && (() => {
        const sorted = [...domainAction.confirmations].sort(
          (a, b) => (a.created_at ?? 0) - (b.created_at ?? 0),
        );
        const proposerCid = sorted[0]?.contract_id;
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
                return (
                  <Box
                    key={c.contract_id}
                    sx={{ display: "flex", alignItems: "center", gap: 0.5 }}
                  >
                    <Typography
                      variant="caption"
                      sx={{
                        fontFamily: "monospace",
                        color: isOwn ? "primary.main" : "text.primary",
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
                    {!isOwn && (
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

      <Box
        sx={{
          display: "flex",
          gap: 1,
          alignItems: "center",
          justifyContent: "flex-end",
        }}
      >
        <Chip
          label={`${domainAction.confirmation_count} / ${party.threshold} confirmed`}
          size="small"
          color={domainAction.can_execute ? "success" : "default"}
          variant={domainAction.can_execute ? "filled" : "outlined"}
        />
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
            variant="outlined"
            onClick={handleConfirm}
            disabled={busy || !party.rulesContractId}
          >
            Confirm
          </Button>
        )}
        {domainAction.can_execute && (
          <Button
            size="small"
            variant="outlined"
            color="success"
            onClick={handleExecute}
            disabled={busy || !party.rulesContractId}
          >
            Execute
          </Button>
        )}
      </Box>
    </Box>
  );
};

type FeedEntry =
  | { kind: "invitation"; ts: number; invitation: PendingInvitation }
  | {
      kind: "action";
      ts: number;
      party: PartyActions;
      action: GovernanceAction;
    }
  | {
      kind: "domain_action";
      ts: number;
      party: PartyActions;
      domainAction: DomainGovernanceAction;
    }
  | { kind: "workflow"; ts: number; run: WorkflowRun };

const WorkflowRunCard = ({
  run,
  onAfter,
  onSelectParty,
}: {
  run: WorkflowRun;
  onAfter: () => void;
  onSelectParty: (partyId: string) => void;
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
      showSnackbar(err instanceof Error ? err.message : "Failed to cancel");
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
      showSnackbar(err instanceof Error ? err.message : "Failed to dismiss");
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
      showSnackbar(err instanceof Error ? err.message : "Failed to retry");
    } finally {
      setBusy(false);
    }
  };

  const statusLabel =
    run.status === "inprogress"
      ? "in progress"
      : run.status === "completed"
        ? "completed"
        : run.status === "failed"
          ? "failed"
          : run.status === "cancelled"
            ? "cancelled"
            : run.status;

  const statusColor: "default" | "success" | "error" | "warning" | "info" =
    run.status === "completed"
      ? "success"
      : run.status === "failed"
        ? "error"
        : run.status === "cancelled"
          ? "warning"
          : "info";

  const fromLine = run.role === "Coordinator"
    ? "started by you"
    : run.coordinator_name
      ? `from ${run.coordinator_name}`
      : run.coordinator_pubkey
        ? `from ${run.coordinator_pubkey.slice(0, 12)}…${run.coordinator_pubkey.slice(-6)}`
        : null;

  const completedCount = run.completed_peers.length;
  const totalCount = run.expected_peers.length;
  // Per-kind label for the peer progress counter. DARs distribution
  // doesn't sign anything — peers just upload the dar locally and
  // signal completion. Other kinds collect DAML signatures.
  const peerVerb = run.kind === "Dars" ? "uploaded" : "signed";
  // Step counters are only meaningful on the coordinator side — the
  // peer's `current_step` is always "Active" with step_index=0,
  // step_total=N (see invitations.rs `upsert_peer_run`), so rendering
  // it as "Active (1/N)" is misleading. Hide step + peer count rows
  // entirely for peer-side cards.
  const isCoordinator = run.role === "Coordinator";

  return (
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
        <Box>
          <Typography variant="subtitle2" sx={{ fontWeight: 600 }}>
            {run.kind} workflow
          </Typography>
          {fromLine && (
            <Typography variant="caption" color="text.secondary">
              {fromLine}
            </Typography>
          )}
        </Box>
        <Box
          sx={{
            display: "flex",
            flexDirection: "column",
            alignItems: "flex-end",
            gap: 0.5,
          }}
        >
          <Chip label={statusLabel} size="small" color={statusColor} />
          <Typography variant="caption" color="text.secondary">
            {formatRelativeTime(run.updated_at)}
          </Typography>
        </Box>
      </Box>

      {(isInProgress || run.error) && (
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
          {isInProgress && (
            <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Step
              </Typography>
              <Typography variant="body2" sx={{ fontWeight: 600 }}>
                {run.current_step} ({run.step_index + 1}/{run.step_total})
              </Typography>
            </Box>
          )}
          {isInProgress && isCoordinator && totalCount > 0 && (
            <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Peers
              </Typography>
              <Typography variant="body2" sx={{ fontWeight: 600 }}>
                {completedCount} / {totalCount} {peerVerb}
              </Typography>
            </Box>
          )}
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
                  fontFamily: "monospace",
                  color: "primary.main",
                  cursor: "pointer",
                  "&:hover": { textDecoration: "underline" },
                }}
              >
                {truncatePartyId(run.dec_party_id)}
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
      )}

      <Box sx={{ display: "flex", gap: 1, justifyContent: "flex-end" }}>
        {isInProgress && run.role === "Coordinator" && (
          <Button
            variant="outlined"
            color="error"
            size="small"
            onClick={cancel}
            disabled={busy}
            startIcon={busy ? <CircularProgress size={14} /> : undefined}
          >
            Cancel Workflow
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
      </Box>
    </Box>
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
  if (loading) {
    return (
      <Box sx={{ display: "flex", flexDirection: "column", gap: 1, p: 3 }}>
        <NotificationSkeleton />
        <NotificationSkeleton />
        <NotificationSkeleton />
      </Box>
    );
  }

  const feed: FeedEntry[] = [
    ...pendingInvitations.map<FeedEntry>((invitation) => ({
      kind: "invitation",
      ts: invitation.received_at,
      invitation,
    })),
    ...partyActions.flatMap<FeedEntry>((party) =>
      party.actions.map((action) => ({
        kind: "action",
        ts: action.last_confirmation_at ?? 0,
        party,
        action,
      })),
    ),
    ...partyActions.flatMap<FeedEntry>((party) =>
      party.domainActions.map((domainAction) => ({
        kind: "domain_action",
        // Domain proposals don't carry a server-side timestamp; fall back to
        // the latest confirmation we know about, then 0 for unconfirmed
        // proposals (they sort to the bottom of the feed).
        ts: domainAction.confirmations.reduce(
          (max, c) => Math.max(max, c.created_at ?? 0),
          0,
        ),
        party,
        domainAction,
      })),
    ),
    ...workflowRuns.map<FeedEntry>((run) => ({
      kind: "workflow",
      ts: run.updated_at,
      run,
    })),
  ];
  feed.sort((a, b) => b.ts - a.ts);

  if (feed.length === 0) {
    return (
      <Box sx={{ p: 4, textAlign: "center" }}>
        <Typography variant="body2" color="text.secondary">
          No pending notifications.
        </Typography>
      </Box>
    );
  }

  return (
    <Box sx={{ display: "flex", flexDirection: "column", gap: 1, p: 3 }}>
      {feed.map((entry) => {
        if (entry.kind === "invitation") {
          return (
            <InvitationCard
              key={`inv-${entry.invitation.id}`}
              invitation={entry.invitation}
              onAfter={onInvitationsChanged}
            />
          );
        }
        if (entry.kind === "workflow") {
          return (
            <WorkflowRunCard
              key={`wf-${entry.run.instance_name}`}
              run={entry.run}
              onAfter={onWorkflowsChanged}
              onSelectParty={onSelectParty}
            />
          );
        }
        if (entry.kind === "domain_action") {
          return (
            <DomainActionCard
              key={`dom-${entry.party.partyId}-${entry.domainAction.proposal_cid}`}
              party={entry.party}
              domainAction={entry.domainAction}
              onAfter={onActionsChanged}
              onSelectParty={onSelectParty}
            />
          );
        }
        return (
          <ActionCard
            key={`act-${entry.party.partyId}-${entry.action.action_hash}`}
            party={entry.party}
            action={entry.action}
            onAfter={onActionsChanged}
            onSelectParty={onSelectParty}
          />
        );
      })}
    </Box>
  );
};
