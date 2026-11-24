import { Box, Typography } from "@mui/material";
import { RowCard } from "../RowCard";
import type { CompletedProposal, ProposalOutcome } from "../../completedProposals";
import { Pill } from "../viz/ApprovalCard";
import { workflowKindLabel } from "../../workflowSteps";
import type { HandledInvitation } from "../../types";
import { formatRelativeTime, truncatePartyId } from "./format";

const OUTCOME_PILLS: Record<
  ProposalOutcome,
  { label: string; tone: "success" | "neutral" }
> = {
  executed: { label: "Executed", tone: "success" },
  cancelled: { label: "Cancelled", tone: "neutral" },
  proposed: { label: "You proposed", tone: "neutral" },
  confirmed: { label: "You confirmed", tone: "neutral" },
};

/**
 * A finished proposal in the Completed section, in the same row shape as a
 * finished workflow run. "You proposed" and "You confirmed" are this node's
 * last recorded action: the node logs only what it did itself, so an execute
 * by another member, or an expiry, does not appear.
 */
const CompletedProposalRow = ({ proposal }: { proposal: CompletedProposal }) => {
  const pill = OUTCOME_PILLS[proposal.outcome];
  const endedElsewhere =
    proposal.outcome === "proposed" || proposal.outcome === "confirmed";
  return (
    <RowCard
      dataAttrs={{
        "data-testid": "completed-proposal",
        "data-outcome": proposal.outcome,
      }}
    >
      <Typography
        title={proposal.label}
        sx={{
          fontSize: 14,
          fontWeight: 500,
          whiteSpace: "nowrap",
          overflow: "hidden",
          textOverflow: "ellipsis",
          flexShrink: 0,
          width: 168,
        }}
      >
        {proposal.label}
      </Typography>
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
        {truncatePartyId(proposal.partyId)}
        {endedElsewhere && " · no longer pending"}
      </Typography>
      <Box sx={{ width: 96, flexShrink: 0 }}>
        <Pill label={pill.label} tone={pill.tone} />
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
        {formatRelativeTime(proposal.ts)}
      </Typography>
      {/* Empty actions column, so the row lines up with workflow rows. */}
      <Box sx={{ width: 128, flexShrink: 0 }} />
    </RowCard>
  );
};

/**
 * An accepted or declined invitation in the Completed section, in the same row
 * shape as a finished workflow run.
 */
const HandledInvitationRow = ({ handled }: { handled: HandledInvitation }) => {
  const { invitation, outcome } = handled;
  const from = `from ${
    invitation.coordinator_name ||
    `${invitation.coordinator_pubkey.slice(0, 12)}…${invitation.coordinator_pubkey.slice(-6)}`
  }`;
  return (
    <RowCard
      dataAttrs={{
        "data-testid": "handled-invitation",
        "data-kind": invitation.invitation_type,
        "data-outcome": outcome,
      }}
    >
      <Typography
        sx={{
          fontSize: 14,
          fontWeight: 500,
          whiteSpace: "nowrap",
          overflow: "hidden",
          textOverflow: "ellipsis",
          flexShrink: 0,
          width: 168,
        }}
      >
        {workflowKindLabel(invitation.invitation_type)} invitation
      </Typography>
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
        {invitation.prefix ||
          (invitation.dec_party_id ? truncatePartyId(invitation.dec_party_id) : from)}
      </Typography>
      <Box sx={{ width: 96, flexShrink: 0 }}>
        {outcome === "accepted" ? (
          <Pill label="Accepted" tone="success" />
        ) : (
          <Pill label="Declined" tone="neutral" />
        )}
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
        {formatRelativeTime(handled.handled_at)}
      </Typography>
      {/* Empty actions column, so the row lines up with workflow rows. */}
      <Box sx={{ width: 128, flexShrink: 0 }} />
    </RowCard>
  );
};

export { CompletedProposalRow, HandledInvitationRow };
