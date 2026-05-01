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
  ExecuteActionRequest,
  ExpireConfirmationRequest,
  GovernanceAction,
  GovernanceType,
  PendingInvitation,
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
}

interface NotificationsViewProps {
  pendingInvitations: PendingInvitation[];
  partyActions: PartyActions[];
  /** True while either feed source is still loading its initial response. */
  loading: boolean;
  onInvitationsChanged: () => void;
  onActionsChanged: () => void;
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
          variant="contained"
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

type FeedEntry =
  | { kind: "invitation"; ts: number; invitation: PendingInvitation }
  | {
      kind: "action";
      ts: number;
      party: PartyActions;
      action: GovernanceAction;
    };

export const NotificationsView = ({
  pendingInvitations,
  partyActions,
  loading,
  onInvitationsChanged,
  onActionsChanged,
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
