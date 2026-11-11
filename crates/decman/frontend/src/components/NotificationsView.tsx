import { Fragment, useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { Box, CircularProgress, Skeleton, Typography } from "@mui/material";
import { columnSx } from "../styles";
import { PaginationControls } from "./Pagination";
import { usePagination } from "../usePagination";
import type { AuditLogEntry, HandledInvitation, PendingInvitation, WorkflowRun } from "../types";
import type { PartyActions } from "./notifications/types";
import { InvitationCard } from "./notifications/InvitationCard";
import { ActionCard } from "./notifications/ActionCard";
import { DomainActionCard } from "./notifications/DomainActionCard";
import { WorkflowRunCard } from "./notifications/WorkflowRunCard";
import { CompletedProposalRow, HandledInvitationRow } from "./notifications/CompletedRows";
import {
  buildEntries,
  filterEntries,
  partyEntryCounts,
  type EntryItem,
  type Group,
  type Kind,
} from "./notifications/buildEntries";
import { PartyFilterPicker } from "./notifications/PartyFilterPicker";

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

  // The card or row each kind of entry shows.
  const renderEntry = (item: EntryItem): ReactNode => {
    switch (item.type) {
      case "invitation": {
        const { invitation } = item;
        return (
          <InvitationCard
            invitation={invitation}
            onAfter={onInvitationsChanged}
            onSelectParty={onSelectParty}
          />
        );
      }
      case "action": {
        const { party, action } = item;
        return (
          <ActionCard
            party={party}
            action={action}
            onAfter={onActionsChanged}
            onSelectParty={onSelectParty}
          />
        );
      }
      case "domainAction": {
        const { party, domainAction } = item;
        return (
          <DomainActionCard
            party={party}
            domainAction={domainAction}
            onAfter={onActionsChanged}
            onSelectParty={onSelectParty}
          />
        );
      }
      case "workflowRun": {
        const { run, terminal } = item;
        return (
          <WorkflowRunCard
            run={run}
            onAfter={onWorkflowsChanged}
            onSelectParty={onSelectParty}
            compact={terminal}
          />
        );
      }
      case "completedProposal": {
        const { proposal } = item;
        return <CompletedProposalRow proposal={proposal} />;
      }
      case "handledInvitation": {
        const { handled } = item;
        return <HandledInvitationRow handled={handled} />;
      }
    }
  };

  // Built above the loading / empty early-returns so the pagination hook below
  // runs on every render, loading or not.
  const entries = buildEntries({
    pendingInvitations,
    partyActions,
    workflowRuns,
    governanceAudit,
    handledInvitations,
    pendingLive,
    auditFirstSeen,
    pendingRequestedAt,
  });

  const { groupCount, grouped, anyVisible } = filterEntries(
    entries,
    partyFilter,
    statusFilter,
    typeFilter,
  );

  // "Needs your action" and "In progress" are bounded by how much work is live;
  // only "Completed" grows without limit, so that's the section that pages.
  const {
    page: donePage,
    setPage: setDonePage,
    pageCount: donePageCount,
    pageItems: donePageItems,
    total: doneTotal,
  } = usePagination(grouped.done);

  // Parties with something in the feed, most entries first. Counted from the
  // unfiltered set so a chip's number does not change as you select.
  const partyOptions = partyEntryCounts(entries);
  const partyPicker = (
    <PartyFilterPicker
      partyOptions={partyOptions}
      partyFilter={partyFilter}
      setPartyFilter={setPartyFilter}
      partyPickerOpen={partyPickerOpen}
      setPartyPickerOpen={setPartyPickerOpen}
      partySearch={partySearch}
      setPartySearch={setPartySearch}
    />
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
                  <Fragment key={e.key}>{renderEntry(e.item)}</Fragment>
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
