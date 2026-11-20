import { useState } from "react";
import { Box, Button, IconButton, Tooltip, Typography } from "@mui/material";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import { API_BASE } from "../../constants";
import { authenticatedFetch } from "../../api";
import { copyToClipboard } from "../../clipboard";
import { useSnackbar } from "../../contexts";
import { formatActionDetails, formatActionType } from "../../governanceFormat";
import { ExecuteDialog } from "../ExecuteDialog";
import { ApprovalCard, Pill } from "../viz/ApprovalCard";
import { ConfirmAvatars, ConfirmRing } from "../viz/ConfirmRing";
import { FullPayload } from "../viz/FullPayload";
import type { CancelConfirmationRequest, ConfirmActionRequest, DisclosedContractInput, ExecuteActionRequest, GovernanceAction } from "../../types";
import { formatRelativeTime, truncatePartyId } from "./format";
import type { PartyActions } from "./types";

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
        confirmation_cids: action.executable_confirmation_cids,
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

  const details = formatActionDetails(
    action.action,
    party.threshold,
    party.domainActions,
  );
  const cancelledCid =
    action.action.type === "governance_cancel_action"
      ? action.action.proposal_cid
      : undefined;
  const cancelledProposal = cancelledCid
    ? party.domainActions.find((d) => d.proposal_cid === cancelledCid)
    : undefined;
  const needsYou = action.can_execute || !ownConfirmation;

  return (
    <>
      <ApprovalCard
        accent={needsYou}
        // A pill only where the card offers no way forward. Execute already
        // says quorum is met, and Confirm already says a vote is wanted;
        // "Awaiting others" is the one state no button accounts for.
        pill={
          !action.can_execute && ownConfirmation ? (
            <Pill label="Awaiting others" tone="neutral" />
          ) : undefined
        }
        time={
          action.last_confirmation_at
            ? formatRelativeTime(action.last_confirmation_at)
            : undefined
        }
        title={
          <>
            {formatActionType(action.action).replace(/Governance /i, "")}
            {details[0] && (
              <Box
                component="span"
                sx={{
                  ml: 0.75,
                  fontFamily: "var(--font-mono)",
                  color: "text.primary",
                }}
              >
                {details[0].before !== undefined
                  ? `${details[0].before} → ${details[0].after}`
                  : details[0].after}
              </Box>
            )}
          </>
        }
        facts={
          <Box
            sx={{
              display: "flex",
              flexWrap: "wrap",
              alignItems: "center",
              gap: "6px 16px",
              fontSize: 13,
              color: "text.secondary",
            }}
          >
            <Box sx={{ display: "flex", alignItems: "center", gap: 0.5 }}>
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
            {details.slice(1).map((d, i) => (
              <Box component="span" key={i}>
                {d.label}{" "}
                <Box
                  component="span"
                  sx={{ fontFamily: "var(--font-mono)", color: "text.primary" }}
                >
                  {d.before !== undefined ? `${d.before} → ${d.after}` : d.after}
                </Box>
              </Box>
            ))}
          </Box>
        }
        footerLeft={
          <Box sx={{ display: "flex", alignItems: "center", gap: 1.5 }}>
            <ConfirmRing
              count={action.confirmation_count}
              threshold={party.threshold}
              canExecute={action.can_execute}
            />
            <ConfirmAvatars
              confirmations={action.confirmations}
              memberPartyId={party.memberPartyId}
              threshold={party.threshold}
            />
          </Box>
        }
        actions={
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
              !action.can_execute && (
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
          <>
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
              Exactly what you're signing
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
              {details.map((d, i) => (
                <Box
                  key={i}
                  sx={{ display: "flex", alignItems: "baseline", gap: 1 }}
                >
                  <Typography
                    variant="caption"
                    color="text.secondary"
                    sx={{ minWidth: 110 }}
                  >
                    {d.label}
                  </Typography>
                  <Typography variant="body2" sx={{ fontFamily: "var(--font-mono)" }}>
                    {d.before !== undefined ? (
                      <>
                        <Box
                          component="span"
                          sx={{ color: "text.disabled" }}
                        >
                          {d.before}
                        </Box>{" "}
                        → {d.after}
                      </>
                    ) : (
                      d.after
                    )}
                  </Typography>
                </Box>
              ))}
              <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
                <Typography
                  variant="caption"
                  color="text.secondary"
                  sx={{ minWidth: 110 }}
                >
                  Governance type
                </Typography>
                <Typography variant="body2" sx={{ fontFamily: "var(--font-mono)" }}>
                  {party.governanceType}
                </Typography>
              </Box>
              <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
                <Typography
                  variant="caption"
                  color="text.secondary"
                  sx={{ minWidth: 110 }}
                >
                  Action hash
                </Typography>
                <Typography variant="body2" sx={{ fontFamily: "var(--font-mono)" }}>
                  {action.action_hash.slice(0, 12)}…
                </Typography>
                <Tooltip title="Copy action hash">
                  <IconButton
                    size="small"
                    onClick={async () => {
                      const ok = await copyToClipboard(action.action_hash);
                      showSnackbar(ok ? "Copied to clipboard" : "Failed to copy");
                    }}
                    sx={{ p: 0.25 }}
                  >
                    <ContentCopyIcon sx={{ fontSize: 14 }} />
                  </IconButton>
                </Tooltip>
              </Box>
            </Box>
            {cancelledProposal?.description && (
              <Box sx={{ mt: 1.5 }}>
                <Typography variant="caption" color="text.secondary">
                  Description of the proposal this vote cancels
                </Typography>
                <Typography
                  variant="body2"
                  sx={{ mt: 0.5, px: 1.25, py: 1, bgcolor: "action.hover", borderRadius: 1 }}
                >
                  {cancelledProposal.description}
                </Typography>
              </Box>
            )}
            <Box sx={{ mt: 1.5 }}>
              <FullPayload value={action.action} />
            </Box>
          </>
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

export { ActionCard };
