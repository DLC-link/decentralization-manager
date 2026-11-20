import { Fragment, useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { Box, Button, Checkbox, CircularProgress, Dialog, DialogActions, DialogContent, DialogTitle, List, ListItemButton, ListItemText, Skeleton, TextField, Typography } from "@mui/material";
import { columnSx } from "../styles";
import { PaginationControls } from "./Pagination";
import { usePagination } from "../usePagination";
import { completedProposals } from "../completedProposals";
import type { AuditLogEntry, HandledInvitation, PendingInvitation, WorkflowRun } from "../types";
import type { PartyActions } from "./notifications/types";
import { InvitationCard } from "./notifications/InvitationCard";
import { ActionCard } from "./notifications/ActionCard";
import { DomainActionCard } from "./notifications/DomainActionCard";
import { WorkflowRunCard } from "./notifications/WorkflowRunCard";
import { CompletedProposalRow, HandledInvitationRow } from "./notifications/CompletedRows";

export { type PartyActions } from "./notifications/types";

interface NotificationsViewProps {
  pendingInvitations: PendingInvitation[];
  partyActions: PartyActions[];
  /** Live + recently-terminal workflow runs (coordinator-side for now). */
  workflowRuns: WorkflowRun[];
  /** This node's newest governance audit rows across every party. The
   *  Completed section reads finished proposals from them. */
  governanceAudit?: AuditLogEntry[];
  /** Invitations this node accepted or declined, newest first. */
  handledInvitations?: HandledInvitation[];
  /** False while `partyActions` is not being refreshed, as after a load-more.
   *  Completed then shows only executed and cancelled proposals. */
  pendingLive?: boolean;
  /** When the page first saw each audit row, by row id (`performance.now()`). */
  auditFirstSeen?: ReadonlyMap<number, number>;
  /** When the committed `partyActions` was requested (`performance.now()`).
   *  With `auditFirstSeen`, Completed calls a proposal "no longer pending"
   *  only from a pending feed requested after its audit row was seen. */
  pendingRequestedAt?: number;
  /** True while any feed source is still loading its initial response. */
  loading: boolean;
  /** True while a refetch is in flight, initial or not. Keeps an empty list
   *  from reading as "nothing to approve" when the answer is not in yet. */
  fetching?: boolean;
  /** Pull the next batch from the ledger. Called when the foot of the list
   *  scrolls into view. */
  onLoadMore?: () => void;
  /** Whether any party still has a batch left. */
  hasMore?: boolean;
  /** True while a later batch is in flight. */
  loadingMore?: boolean;
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

export const NotificationsView = ({
  pendingInvitations,
  partyActions,
  workflowRuns,
  governanceAudit = [],
  handledInvitations = [],
  pendingLive = true,
  auditFirstSeen,
  pendingRequestedAt,
  loading,
  fetching = false,
  onLoadMore,
  hasMore = false,
  loadingMore = false,
  onInvitationsChanged,
  onActionsChanged,
  onWorkflowsChanged,
  onSelectParty,
}: NotificationsViewProps) => {
  const sentinelObserver = useRef<IntersectionObserver | null>(null);
  const loadMoreRef = useRef<(() => void) | undefined>(onLoadMore);
  useEffect(() => {
    loadMoreRef.current = onLoadMore;
  }, [onLoadMore]);

  // A callback ref, not an effect: the sentinel renders below several early
  // returns, and a hook placed after those changes the hook count between
  // renders (React #310). This attaches when the node mounts instead.
  const attachSentinel = useCallback((node: HTMLDivElement | null) => {
    sentinelObserver.current?.disconnect();
    sentinelObserver.current = null;
    if (!node) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) loadMoreRef.current?.();
      },
      { rootMargin: "300px" },
    );
    observer.observe(node);
    sentinelObserver.current = observer;
  }, []);

  const [statusFilter, setStatusFilter] = useState<
    "all" | "you" | "prog" | "done"
  >("all");
  const [typeFilter, setTypeFilter] = useState<"all" | "gov" | "wf" | "inv">(
    "all",
  );

  // Empty means every party. Declared with the other filters so the hook
  // count never varies between renders.
  const [partyFilter, setPartyFilter] = useState<string[]>([]);
  const [partyPickerOpen, setPartyPickerOpen] = useState(false);
  const [partySearch, setPartySearch] = useState("");
  const [doneCollapsed, setDoneCollapsed] = useState(true);

  // Built above the loading / empty early-returns so the pagination hook below
  // runs on every render, loading or not.
  // ── Categorize every feed item by what it needs from the operator ──
  type Group = "you" | "prog" | "done";
  type Kind = "gov" | "wf" | "inv";
  interface Entry {
    key: string;
    group: Group;
    kind: Kind;
    ts: number;
    node: ReactNode;
    /** Dec party this entry belongs to. Absent for invitations and workflow
     *  runs, which carry no party, so the party filter leaves those alone. */
    partyId?: string;
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
        partyId: party.partyId,
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
      node: <CompletedProposalRow proposal={proposal} />,
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
      node: <HandledInvitationRow handled={handled} />,
    });
  }

  entries.sort((a, b) => b.ts - a.ts);

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

  // "Needs your action" and "In progress" are bounded by how much work is live;
  // only "Completed" grows without limit, so that's the section that pages.
  const {
    page: donePage,
    setPage: setDonePage,
    pageCount: donePageCount,
    pageItems: donePageItems,
    total: doneTotal,
  } = usePagination(grouped.done);

  // Parties with something in the feed, newest activity first. Counted from the
  // unfiltered set so a chip's number does not change as you select.
  const partyCounts = new Map<string, number>();
  for (const e of entries) {
    if (e.partyId) {
      partyCounts.set(e.partyId, (partyCounts.get(e.partyId) ?? 0) + 1);
    }
  }
  const partyOptions = [...partyCounts.entries()].sort((a, b) => b[1] - a[1]);

  // At most this many rows are mounted at once. A deployment can hold tens of
  // thousands of parties, and mounting a row each would stall the dialog.
  const PARTY_ROWS_SHOWN = 200;
  const searchLower = partySearch.trim().toLowerCase();
  const matchingParties = partyOptions.filter(
    ([partyId]) => !searchLower || partyId.toLowerCase().includes(searchLower),
  );
  const shownParties = matchingParties.slice(0, PARTY_ROWS_SHOWN);

  const partyPicker = (
    <>
      <Button
        size="small"
        variant="outlined"
        onClick={() => setPartyPickerOpen(true)}
        sx={{
          fontFamily: "var(--font-mono)",
          fontSize: 12,
          textTransform: "none",
          whiteSpace: "nowrap",
        }}
      >
        {partyFilter.length === 0
          ? "All parties"
          : `${partyFilter.length} ${partyFilter.length === 1 ? "party" : "parties"}`}
      </Button>

      <Dialog
        open={partyPickerOpen}
        onClose={() => setPartyPickerOpen(false)}
        fullWidth
        maxWidth="sm"
      >
        <DialogTitle sx={{ pb: 1 }}>Filter by dec party</DialogTitle>
        <DialogContent dividers sx={{ pt: 1 }}>
          <TextField
            size="small"
            fullWidth
            autoFocus
            placeholder="Search parties…"
            value={partySearch}
            onChange={(e) => setPartySearch(e.target.value)}
            sx={{ mb: 1 }}
          />
          <Box
            sx={{
              display: "flex",
              gap: 1,
              alignItems: "center",
              mb: 1,
              flexWrap: "wrap",
            }}
          >
            <Button
              size="small"
              onClick={() =>
                setPartyFilter(matchingParties.map(([partyId]) => partyId))
              }
              disabled={matchingParties.length === 0}
            >
              Select all{searchLower ? " matching" : ""}
            </Button>
            <Button
              size="small"
              onClick={() => setPartyFilter([])}
              disabled={partyFilter.length === 0}
            >
              Deselect all
            </Button>
            <Box sx={{ flex: 1 }} />
            <Typography variant="caption" color="text.secondary">
              {partyFilter.length === 0
                ? "No filter: every party shown"
                : `${partyFilter.length} selected`}
            </Typography>
          </Box>

          <List dense disablePadding sx={{ maxHeight: 360, overflowY: "auto" }}>
            {shownParties.map(([partyId, count]) => {
              const checked = partyFilter.includes(partyId);
              return (
                <ListItemButton
                  key={partyId}
                  onClick={() =>
                    setPartyFilter((current) =>
                      current.includes(partyId)
                        ? current.filter((p) => p !== partyId)
                        : [...current, partyId],
                    )
                  }
                  sx={{ borderRadius: 1 }}
                >
                  <Checkbox
                    edge="start"
                    size="small"
                    checked={checked}
                    tabIndex={-1}
                    disableRipple
                  />
                  <ListItemText
                    primary={
                      <Box
                        component="span"
                        sx={{
                          display: "block",
                          fontFamily: "var(--font-mono)",
                          fontSize: 13,
                          overflow: "hidden",
                          textOverflow: "ellipsis",
                          whiteSpace: "nowrap",
                        }}
                      >
                        {partyId.split("::")[0]}
                      </Box>
                    }
                    secondary={`${count} ${count === 1 ? "entry" : "entries"}`}
                  />
                </ListItemButton>
              );
            })}
          </List>

          {matchingParties.length > shownParties.length && (
            <Typography
              variant="caption"
              color="text.secondary"
              sx={{ display: "block", mt: 1 }}
            >
              Showing {shownParties.length} of {matchingParties.length}. Narrow
              the search to reach the rest.
            </Typography>
          )}
          {matchingParties.length === 0 && (
            <Typography
              variant="body2"
              color="text.secondary"
              sx={{ py: 2, textAlign: "center" }}
            >
              No party matches that search.
            </Typography>
          )}
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setPartyPickerOpen(false)}>Done</Button>
        </DialogActions>
      </Dialog>
    </>
  );

  if (loading) {
    return (
      <Box sx={{ display: "flex", flexDirection: "column", gap: 1, p: 3 }}>
        <NotificationSkeleton />
        <NotificationSkeleton />
        <NotificationSkeleton />
      </Box>
    );
  }

  if (entries.length === 0) {
    if (fetching) {
      return (
        <Box sx={{ display: "flex", flexDirection: "column", gap: 1, p: 3 }}>
          <Typography variant="body2" color="text.secondary" sx={{ px: 1 }}>
            Querying proposals…
          </Typography>
          <NotificationSkeleton />
          <NotificationSkeleton />
        </Box>
      );
    }
    return (
      <Box sx={{ p: 4, textAlign: "center" }}>
        <Typography variant="body2" color="text.secondary" sx={{ mb: 2 }}>
          {partyFilter.length > 0
            ? "No notifications for the selected parties."
            : "No pending notifications."}
        </Typography>
        {/* Kept reachable here: a filter that matches nothing would otherwise
          * be impossible to clear from this screen. */}
        <Box sx={{ display: "inline-flex" }}>{partyPicker}</Box>
      </Box>
    );
  }

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

  // Whether the Completed footer bar shows. Decided here rather than inside the
  // section loop because the bar renders *outside* the capped column, so its
  // rule runs the full width of the view — the same shape the parties lists use.
  const doneShown =
    (statusFilter === "all" || statusFilter === "done") &&
    grouped.done.length > 0 &&
    !(doneCollapsed && statusFilter !== "done");
  const showDoneFooter = doneShown && donePageCount > 1;

  const loadedCount = sections
    .filter(({ group }) => group !== "done")
    .reduce((sum, { group }) => sum + grouped[group].length, 0);


  return (
    <Box sx={{ flex: 1, display: "flex", flexDirection: "column" }}>
      {/* No top padding — the sticky filter bar belongs at the top of the view
        * — but the last card still needs to clear the bottom of the page. With
        * the Completed footer showing, this is the gutter above it; the bar
        * itself is a sibling, so it stays flush. */}
      <Box sx={{ flex: 1, pb: 3, ...columnSx }}>
      {/* The sticky element is this outer box, and its fill runs on past the
        * rule: cards scroll *under* the bar, so without that strip the next one
        * arrives flush against the rule. A margin cannot do the job — it
        * scrolls away with the content. */}
      <Box
        sx={{
          position: "sticky",
          top: 0,
          zIndex: 5,
          pb: 1.5,
          bgcolor: "background.default",
        }}
      >
      <Box
        sx={{
          display: "flex",
          flexWrap: "wrap",
          alignItems: "center",
          gap: 1,
          py: 1.5,
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
        {partyPicker}
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
                {(group === "done" ? donePageItems : grouped[group]).map((e) => (
                  <Fragment key={e.key}>{e.node}</Fragment>
                ))}
              </Box>
            )}
          </Box>
        );
      })}
      </Box>

      {/* Grows the mounted count as it scrolls into view, so a long queue
        * arrives a batch at a time instead of all at once. */}
      {hasMore && (
        <Box
          ref={attachSentinel}
          sx={{
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            gap: 1,
            py: 2,
          }}
        >
          {loadingMore && <CircularProgress size={14} />}
          <Typography variant="caption" color="text.secondary">
            {loadingMore
              ? `Loading more… (${loadedCount} so far)`
              : `Scroll for more (${loadedCount} loaded)`}
          </Typography>
        </Box>
      )}

      {/* Outside the column, so the rule runs the full width of the view and
        * the bar sits flush at the bottom — the parties lists' footer shape. */}
      {showDoneFooter && (
        <PaginationControls
          page={donePage}
          pageCount={donePageCount}
          total={doneTotal}
          onChange={setDonePage}
        />
      )}
    </Box>
  );
};
