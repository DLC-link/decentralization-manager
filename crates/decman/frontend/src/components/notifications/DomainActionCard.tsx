import { useState } from "react";
import { Alert, Box, Button, Dialog, DialogActions, DialogContent, DialogTitle, IconButton, Tooltip, Typography } from "@mui/material";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import { API_BASE } from "../../constants";
import { authenticatedFetch } from "../../api";
import { copyToClipboard } from "../../clipboard";
import { useSnackbar } from "../../contexts";
import { ApprovalCard, Pill } from "../viz/ApprovalCard";
import { ConfirmAvatars, ConfirmRing } from "../viz/ConfirmRing";
import { FullPayload } from "../viz/FullPayload";
import type { DomainGovernanceAction } from "../../types";
import { formatRelativeTime, formatTimeUntil, truncatePartyId } from "./format";
import type { PartyActions } from "./types";

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
  const otherConfirmations = domainAction.confirmations.filter(
    (c) => c.confirming_party !== party.memberPartyId,
  );
  const nowSeconds = Math.floor(Date.now() / 1000);
  // GovernanceConfirmation_Expire refuses to run before a confirmation's
  // expiry time, and only its own confirmer can archive it any earlier. So
  // another member's live confirmation is nobody else's to clear yet.
  const expiredOthers = otherConfirmations.filter(
    (c) => (c.expires_at ?? 0) > 0 && (c.expires_at ?? 0) <= nowSeconds,
  );
  const nextOtherExpiry = otherConfirmations
    .map((c) => c.expires_at ?? 0)
    .filter((t) => t > nowSeconds)
    .sort((a, b) => a - b)[0];
  // Only the proposer controls GovernableAction_ProposerCancel, so anyone else
  // pressing the button would just collect a ledger rejection.
  const canCancelProposal =
    !!domainAction.proposer && domainAction.proposer === party.memberPartyId;
  // A cancellation vote is a self-action, so it shows up among the party's
  // self-action cards while members vote on it.
  const cancellationOpen = party.actions.some(
    (a) =>
      a.action.type === "governance_cancel_action" &&
      a.action.proposal_cid === domainAction.proposal_cid,
  );
  // The ledger rejects every confirmation of this proposal, so members vote
  // to archive it instead. Its proposer can retract it without a vote.
  const canProposeCancellation =
    domainAction.proposer_not_authorized &&
    !domainAction.orphaned &&
    !canCancelProposal &&
    !cancellationOpen &&
    !!party.memberPartyId;
  const [cancelDialogOpen, setCancelDialogOpen] = useState(false);

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
  ): Promise<boolean> => {
    // Only the endpoints that exercise a choice on the rules contract need it.
    // Cancelling a confirmation or a proposal goes straight to the contract
    // being archived, so it stays available when the rules cid is unknown.
    const needsRulesContract =
      typeof body === "object" && body !== null && "rules_contract_id" in body;
    if (needsRulesContract && !party.rulesContractId) {
      showSnackbar("Governance rules contract is not set", "error");
      return false;
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

  // Retract the proposal, and archive our own confirmation with it. The server
  // sends both as one transaction, because nobody else can clear that
  // confirmation once the proposal it points at is gone.
  const handleCancelProposal = () =>
    post(
      "cancel-proposal",
      {
        party_id: party.partyId,
        proposal_cid: domainAction.proposal_cid,
        confirmation_cid: ownConfirmation?.contract_id,
      },
      "Proposal cancelled",
    );

  // The first member confirmation of a self-action proposes it.
  const handleProposeCancellation = async () => {
    const proposed = await post(
      "confirm",
      {
        party_id: party.partyId,
        rules_contract_id: party.rulesContractId,
        action: {
          type: "governance_cancel_action" as const,
          proposal_cid: domainAction.proposal_cid,
        },
        governance_type: "core_self" as const,
      },
      "Cancellation proposed",
    );
    if (proposed) setCancelDialogOpen(false);
  };

  // For orphaned actions (underlying proposal is gone) the only valid
  // operation is to clear the stranded Confirmation contracts off the ledger.
  // Our own goes through Revoke instead, and another member's live one is not
  // ours to clear, so this covers the expired remainder. Loop sequentially so
  // the `busy` lock on each /governance/expire call serializes correctly.
  const handleDismissOrphan = async () => {
    for (const c of expiredOthers) {
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
        confirmation_cids: domainAction.executable_confirmation_cids,
        disclosed_contracts: [],
        governance_type: "core_domain" as const,
        proposal_cid: domainAction.proposal_cid,
      },
      "Proposal executed",
    );

  const latestConfirm = domainAction.confirmations.reduce(
    (max, c) => Math.max(max, c.created_at ?? 0),
    0,
  );
  const needsYou =
    domainAction.can_execute || !!domainAction.orphaned || !ownConfirmation;

  return (
    <>
    <ApprovalCard
      accent={needsYou}
      // As on the action card: a pill only where no button says it already.
      // Orphaned and an unauthorized proposer keep one — each is a problem,
      // not a step.
      pill={
        domainAction.orphaned ? (
          <Pill label="Orphaned" tone="danger" />
        ) : domainAction.proposer_not_authorized ? (
          <Pill label="Proposer not authorized" tone="danger" />
        ) : !domainAction.can_execute && ownConfirmation ? (
          <Pill label="Awaiting others" tone="neutral" />
        ) : undefined
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
              display: "inline-flex",
              alignItems: "center",
              gap: 0.5,
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
        <Box sx={{ display: "flex", alignItems: "center", gap: 1.5 }}>
          <ConfirmRing
            count={domainAction.confirmation_count}
            threshold={party.threshold}
            canExecute={domainAction.can_execute}
          />
          <ConfirmAvatars
            confirmations={domainAction.confirmations}
            memberPartyId={party.memberPartyId}
            threshold={party.threshold}
          />
        </Box>
      }
      actions={
        domainAction.orphaned ? (
          <>
            {ownConfirmation && (
              <Button
                size="small"
                variant="outlined"
                color="error"
                onClick={handleRevoke}
                disabled={busy}
              >
                Revoke
              </Button>
            )}
            {otherConfirmations.length > 0 && (
              <Tooltip
                title={
                  expiredOthers.length > 0
                    ? "Clear the other members' expired confirmations"
                    : `Only each member clears their own confirmation until it expires${
                        nextOtherExpiry
                          ? `, and the next one expires ${formatTimeUntil(nextOtherExpiry)}`
                          : ""
                      }`
                }
              >
                <span>
                  <Button
                    size="small"
                    variant="outlined"
                    color="warning"
                    onClick={handleDismissOrphan}
                    disabled={
                      busy || !party.rulesContractId || expiredOthers.length === 0
                    }
                  >
                    Dismiss
                  </Button>
                </span>
              </Tooltip>
            )}
          </>
        ) : (
          <>
            {ownConfirmation ? (
              <Button
                size="small"
                variant="outlined"
                color="error"
                onClick={handleRevoke}
                disabled={busy}
              >
                Revoke
              </Button>
            ) : (
              !domainAction.can_execute &&
              !domainAction.proposer_not_authorized && (
                <Button
                  size="small"
                  variant="contained"
                  color="success"
                  onClick={handleConfirm}
                  disabled={busy || !party.rulesContractId}
                >
                  Confirm
                </Button>
              )
            )}
            {canCancelProposal && (
              <Button
                size="small"
                variant="outlined"
                color="error"
                onClick={handleCancelProposal}
                disabled={busy}
              >
                Cancel proposal
              </Button>
            )}
            {canProposeCancellation && (
              <Button
                size="small"
                variant="outlined"
                color="error"
                onClick={() => setCancelDialogOpen(true)}
                disabled={busy || !party.rulesContractId}
              >
                Propose cancellation
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
          <Typography
            sx={{
              fontFamily: "var(--font-mono)",
              fontSize: 10,
              letterSpacing: "0.12em",
              textTransform: "uppercase",
              color: "text.secondary",
            }}
          >
            Exactly what you're signing
          </Typography>

      {domainAction.orphaned && (
        <Alert severity="warning" sx={{ py: 0.5 }}>
          The underlying proposal has been archived, so these confirmation
          contracts are stranded on the ledger. Each member revokes their own.
          Anyone can dismiss the rest once they expire.
        </Alert>
      )}

      {domainAction.proposer_not_authorized && (
        <Alert severity="warning" sx={{ py: 0.5 }}>
          The governance rules no longer list{" "}
          {domainAction.proposer
            ? truncatePartyId(domainAction.proposer)
            : "the proposer"}{" "}
          as a member or an additional proposer, so the ledger rejects every
          new confirmation of this proposal.{" "}
          {cancellationOpen
            ? "Members are voting to cancel it."
            : "The proposer can cancel it. Members can also vote to cancel it."}
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

      {/* The contract id and template sit beside the arguments so the JSON
          names the exact contract the member acts on. */}
      {domainAction.proposal_payload != null && (
        <FullPayload
          value={{
            contract_id: domainAction.proposal_cid,
            template_id: domainAction.proposal_template_id,
            create_arguments: domainAction.proposal_payload,
          }}
        />
      )}
        </Box>
      }
    />
    <Dialog
      open={cancelDialogOpen}
      onClose={() => setCancelDialogOpen(false)}
      maxWidth="sm"
      fullWidth
    >
      <DialogTitle>Propose cancellation</DialogTitle>
      <DialogContent>
        <Typography variant="body2" sx={{ mb: 2 }}>
          Members vote on this cancellation like any other governance action.
          If the vote passes, the ledger archives this proposal.
        </Typography>
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
          {[
            { label: "Proposal", value: domainAction.action_label },
            { label: "Proposer", value: domainAction.proposer ?? "Unknown" },
            {
              label: "Description",
              value: domainAction.description || "No description",
            },
            { label: "Contract", value: domainAction.proposal_cid },
          ].map((row) => (
            <Box key={row.label} sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography variant="caption" color="text.secondary" sx={{ minWidth: 90 }}>
                {row.label}
              </Typography>
              <Typography variant="body2" sx={{ overflowWrap: "anywhere" }}>
                {row.value}
              </Typography>
            </Box>
          ))}
        </Box>
      </DialogContent>
      <DialogActions>
        <Button onClick={() => setCancelDialogOpen(false)} disabled={busy}>
          Back
        </Button>
        <Button
          variant="contained"
          color="error"
          onClick={handleProposeCancellation}
          disabled={busy || !party.rulesContractId}
        >
          Propose cancellation
        </Button>
      </DialogActions>
    </Dialog>
    </>
  );
};

export { DomainActionCard };
