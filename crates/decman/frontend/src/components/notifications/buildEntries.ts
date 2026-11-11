import { completedProposals, type CompletedProposal } from "../../completedProposals";
import type {
  AuditLogEntry,
  DomainGovernanceAction,
  GovernanceAction,
  HandledInvitation,
  PendingInvitation,
  WorkflowRun,
} from "../../types";
import type { PartyActions } from "./types";

// The notification feed as data: every feed item becomes an entry, grouped
// by what it needs from the operator. NotificationsView renders the cards.

type Group = "you" | "prog" | "done";
type Kind = "gov" | "wf" | "inv";

/** What an entry shows. NotificationsView renders the card for it. */
type EntryItem =
  | { type: "invitation"; invitation: PendingInvitation }
  | { type: "action"; party: PartyActions; action: GovernanceAction }
  | { type: "domainAction"; party: PartyActions; domainAction: DomainGovernanceAction }
  | { type: "workflowRun"; run: WorkflowRun; terminal: boolean }
  | { type: "completedProposal"; proposal: CompletedProposal }
  | { type: "handledInvitation"; handled: HandledInvitation };

interface Entry {
  key: string;
  group: Group;
  kind: Kind;
  ts: number;
  item: EntryItem;
  /** Dec party this entry belongs to. Absent for invitations and workflow
   *  runs, which carry no party, so the party filter leaves those alone. */
  partyId?: string;
}

/** The five feeds NotificationsView shows. */
interface Feeds {
  pendingInvitations: PendingInvitation[];
  partyActions: PartyActions[];
  workflowRuns: WorkflowRun[];
  governanceAudit: AuditLogEntry[];
  handledInvitations: HandledInvitation[];
  pendingLive: boolean;
  auditFirstSeen?: ReadonlyMap<number, number>;
  pendingRequestedAt?: number;
}

/** Every feed item as an entry, newest first. */
const buildEntries = ({
  pendingInvitations,
  partyActions,
  workflowRuns,
  governanceAudit,
  handledInvitations,
  pendingLive,
  auditFirstSeen,
  pendingRequestedAt,
}: Feeds): Entry[] => {
  const entries: Entry[] = [];

  for (const invitation of pendingInvitations) {
    entries.push({
      key: `inv-${invitation.id}`,
      group: "you",
      kind: "inv",
      ts: invitation.received_at,
      item: { type: "invitation", invitation },
    });
  }

  for (const party of partyActions) {
    for (const action of party.actions) {
      const ownConfirmed = action.confirmations.some(
        (c) => c.confirming_party === party.memberPartyId,
      );
      entries.push({
        partyId: party.partyId,
        key: `act-${party.partyId}-${action.action_hash}`,
        // Needs you: you can execute it, or you haven't confirmed yet.
        // In progress: you confirmed and it's waiting on the others.
        group: action.can_execute || !ownConfirmed ? "you" : "prog",
        kind: "gov",
        ts: action.last_confirmation_at ?? 0,
        item: { type: "action", party, action },
      });
    }
    for (const domainAction of party.domainActions) {
      const ownConfirmed = domainAction.confirmations.some(
        (c) => c.confirming_party === party.memberPartyId,
      );
      entries.push({
        partyId: party.partyId,
        key: `dom-${party.partyId}-${domainAction.proposal_cid}`,
        // Orphaned proposals can only be expired — still your cleanup to do.
        group:
          domainAction.can_execute || domainAction.orphaned || !ownConfirmed
            ? "you"
            : "prog",
        kind: "gov",
        // The proposal's own create time, so a card holds its place between
        // refreshes whether or not anyone has confirmed it. An orphaned card
        // has no readable proposal, so fall back to its newest confirmation.
        ts:
          domainAction.created_at ??
          domainAction.confirmations.reduce(
            (max, c) => Math.max(max, c.created_at ?? 0),
            0,
          ),
        item: { type: "domainAction", party, domainAction },
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
      item: { type: "workflowRun", run, terminal },
    });
  }

  // A proposal leaves the pending feed once it is finished, so the Completed
  // section reads it from this node's audit log instead.
  const timing =
    auditFirstSeen && pendingRequestedAt !== undefined
      ? { firstSeen: auditFirstSeen, pendingRequestedAt }
      : undefined;
  for (const proposal of completedProposals(
    governanceAudit,
    partyActions,
    pendingLive,
    timing,
  )) {
    entries.push({
      partyId: proposal.partyId,
      key: `done-${proposal.key}`,
      group: "done",
      kind: "gov",
      ts: proposal.ts,
      item: { type: "completedProposal", proposal },
    });
  }

  // An accepted invitation keeps its own entry beside the workflow run it
  // started: one records the operator's answer, the other the run's outcome.
  // Like a pending invitation it carries no partyId, so the party filter
  // leaves it alone.
  const pendingInvitationIds = new Set(pendingInvitations.map((i) => i.id));
  for (const handled of handledInvitations) {
    // Sent again since it was answered; the pending card stands for it now.
    if (pendingInvitationIds.has(handled.invitation.id)) continue;
    entries.push({
      key: `inv-done-${handled.invitation.id}`,
      group: "done",
      kind: "inv",
      ts: handled.handled_at,
      item: { type: "handledInvitation", handled },
    });
  }

  entries.sort((a, b) => b.ts - a.ts);
  return entries;
};

/**
 * The entries the party, status and type filters keep, by group, and the
 * count each status chip shows.
 */
const filterEntries = (
  entries: Entry[],
  partyFilter: string[],
  statusFilter: "all" | Group,
  typeFilter: "all" | Kind,
) => {
  // Party filter first: the other dimensions' counts should reflect it.
  const byParty = entries.filter(
    (e) =>
      partyFilter.length === 0 ||
      e.partyId === undefined ||
      partyFilter.includes(e.partyId),
  );

  // Each chip's count reflects the *other* active filter dimension.
  const byKind = byParty.filter(
    (e) => typeFilter === "all" || e.kind === typeFilter,
  );
  const groupCount: Record<"all" | Group, number> = {
    all: byKind.length,
    you: byKind.filter((e) => e.group === "you").length,
    prog: byKind.filter((e) => e.group === "prog").length,
    done: byKind.filter((e) => e.group === "done").length,
  };

  const grouped: Record<Group, Entry[]> = { you: [], prog: [], done: [] };
  for (const e of byParty) {
    if (statusFilter !== "all" && statusFilter !== e.group) continue;
    if (typeFilter !== "all" && typeFilter !== e.kind) continue;
    grouped[e.group].push(e);
  }
  const anyVisible =
    grouped.you.length + grouped.prog.length + grouped.done.length > 0;
  return { groupCount, grouped, anyVisible };
};

/** The parties with entries, each with its entry count, most entries first. */
const partyEntryCounts = (entries: Entry[]): [string, number][] => {
  const partyCounts = new Map<string, number>();
  for (const e of entries) {
    if (e.partyId) {
      partyCounts.set(e.partyId, (partyCounts.get(e.partyId) ?? 0) + 1);
    }
  }
  const partyOptions = [...partyCounts.entries()].sort((a, b) => b[1] - a[1]);
  return partyOptions;
};

export { buildEntries, filterEntries, partyEntryCounts };
export type { Entry, EntryItem, Group, Kind };
