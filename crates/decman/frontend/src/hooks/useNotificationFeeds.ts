import { useCallback, useEffect, useRef, useState } from "react";
import { API_BASE } from "../constants";
import { authenticatedFetch } from "../api";
import { useSnackbar } from "../contexts";
import {
  EMPTY_AUDIT_SNAPSHOT,
  nextAuditSnapshot,
  type AuditSnapshot,
} from "../completedProposals";
import { workflowKindLabel } from "../workflowSteps";
import type {
  AuditLogEntry,
  AuditLogResponse,
  HandledInvitation,
  HandledInvitationsResponse,
  PendingInvitation,
  WorkflowRun,
} from "../types";
import { useLatestFetch } from "./useLatestFetch";
import { usePolling } from "./usePolling";

/// Newest audit rows the Completed section reads, across every party. Several
/// rows can belong to one proposal, so this bounds rows, not entries.
const AUDIT_FEED_LIMIT = 200;

/// Newest accepted and declined invitations the Completed section shows.
const HANDLED_INVITATIONS_LIMIT = 50;

/** This node's newest audit rows across every party, or null on failure. */
async function fetchGovernanceAudit(): Promise<AuditLogEntry[] | null> {
  try {
    const res = await authenticatedFetch(
      `${API_BASE}/governance/audit?limit=${AUDIT_FEED_LIMIT}`,
    );
    if (!res.ok) return null;
    const data: Partial<AuditLogResponse> = await res.json();
    return data.entries ?? [];
  } catch {
    // Ignore polling errors
    return null;
  }
}

/**
 * The notification feeds: pending and handled invitations, workflow runs
 * and the governance audit, each polled on its own cadence, plus the toast
 * when a workflow run finishes.
 */
export function useNotificationFeeds() {
  const { showSnackbar } = useSnackbar();
  const [pendingInvitations, setPendingInvitations] = useState<
    PendingInvitation[]
  >([]);
  const [invitationsLoaded, setInvitationsLoaded] = useState(false);
  const [workflowRuns, setWorkflowRuns] = useState<WorkflowRun[]>([]);
  // Request counters for the feeds, so a slower earlier answer cannot
  // overwrite a later one.
  const beginInvitations = useLatestFetch();
  const beginWorkflowRuns = useLatestFetch();
  const beginGovernanceAudit = useLatestFetch();
  const [workflowRunsLoaded, setWorkflowRunsLoaded] = useState(false);
  const [governanceAudit, setGovernanceAudit] =
    useState<AuditSnapshot>(EMPTY_AUDIT_SNAPSHOT);
  const [governanceAuditLoaded, setGovernanceAuditLoaded] = useState(false);
  const beginHandledInvitations = useLatestFetch();
  const [handledInvitations, setHandledInvitations] = useState<
    HandledInvitation[]
  >([]);
  const [handledInvitationsLoaded, setHandledInvitationsLoaded] =
    useState(false);

  const refreshInvitations = useCallback(async () => {
    // Newest request wins. Accepting an invitation refreshes the feed by hand
    // while a poll is already in flight, and that older answer, landing after,
    // would put the invitation back.
    const isLatest = beginInvitations();
    try {
      const res = await authenticatedFetch(`${API_BASE}/invitations`);
      if (!res.ok) return;
      // Compared after the body, not before: reading it is another wait, and a
      // manual refresh that finishes inside it would be overwritten here.
      const data = await res.json();
      if (!isLatest()) return;
      setPendingInvitations(data.invitations);
    } catch {
      // Ignore polling errors
    } finally {
      setInvitationsLoaded(true);
    }
  }, [beginInvitations]);

  // Poll pending invitations every 2 seconds, in-flight-guarded like the
  // status probe above: a slow response must not stack concurrent requests.
  usePolling(refreshInvitations, 2000);

  const refreshWorkflowRuns = useCallback(async () => {
    // Newest request wins, as above: cancel, dismiss and retry all refresh by
    // hand alongside the poll.
    const isLatest = beginWorkflowRuns();
    try {
      const res = await authenticatedFetch(`${API_BASE}/workflows`);
      if (!res.ok) return;
      const data = await res.json();
      if (!isLatest()) return;
      setWorkflowRuns(data.runs ?? []);
    } catch {
      // Ignore polling errors
    } finally {
      setWorkflowRunsLoaded(true);
    }
  }, [beginWorkflowRuns]);

  // Poll workflow_runs every 2 seconds, same cadence as invitations.
  usePolling(refreshWorkflowRuns, 2000);

  const commitGovernanceAudit = useCallback((entries: AuditLogEntry[]) => {
    const now = performance.now();
    setGovernanceAudit((previous) => nextAuditSnapshot(previous, entries, now));
  }, []);

  const refreshGovernanceAudit = useCallback(async () => {
    const isLatest = beginGovernanceAudit();
    try {
      const entries = await fetchGovernanceAudit();
      if (entries === null || !isLatest()) return;
      commitGovernanceAudit(entries);
    } finally {
      setGovernanceAuditLoaded(true);
    }
  }, [beginGovernanceAudit, commitGovernanceAudit]);

  // Same cadence as the governance actions below: the Completed section reads
  // the two together to tell a finished proposal from a pending one.
  usePolling(refreshGovernanceAudit, 10_000);

  const refreshHandledInvitations = useCallback(async () => {
    const isLatest = beginHandledInvitations();
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/invitations/handled?limit=${HANDLED_INVITATIONS_LIMIT}`,
      );
      if (!res.ok) return;
      const data: Partial<HandledInvitationsResponse> = await res.json();
      if (!isLatest()) return;
      setHandledInvitations(data.invitations ?? []);
    } catch {
      // Ignore polling errors
    } finally {
      setHandledInvitationsLoaded(true);
    }
  }, [beginHandledInvitations]);

  // Slower than the pending poll: the list changes when an operator answers,
  // and this page refreshes it by hand when its own operator does.
  usePolling(refreshHandledInvitations, 10_000);

  // An accept or decline moves the invitation from one list to the other.
  const refreshInvitationLists = useCallback(() => {
    void refreshInvitations();
    void refreshHandledInvitations();
  }, [refreshInvitations, refreshHandledInvitations]);

  // Watch for workflow runs transitioning out of `inprogress` and surface a
  // snackbar — the run card in the feed updates too, but a transient toast
  // gives the user a top-level signal without making them go look. First
  // observation of a run is silent so we don't fire on initial page load.
  const prevRunStatusRef = useRef<Map<string, WorkflowRun["status"]>>(new Map());
  useEffect(() => {
    const prev = prevRunStatusRef.current;
    const next = new Map<string, WorkflowRun["status"]>();
    for (const run of workflowRuns) {
      next.set(run.instance_name, run.status);
      const prior = prev.get(run.instance_name);
      if (prior === "inprogress" && run.status !== "inprogress") {
        const label = `${workflowKindLabel(run.kind)} workflow`;
        if (run.status === "failed") {
          showSnackbar(
            `${label} failed: ${run.error || "Unknown error"}`,
            "error",
          );
        } else if (run.status === "completed") {
          showSnackbar(`${label} completed`);
        } else if (run.status === "cancelled") {
          showSnackbar(`${label} cancelled`);
        }
      }
    }
    prevRunStatusRef.current = next;
  }, [workflowRuns, showSnackbar]);

  return {
    pendingInvitations,
    invitationsLoaded,
    workflowRuns,
    workflowRunsLoaded,
    governanceAudit,
    governanceAuditLoaded,
    handledInvitations,
    handledInvitationsLoaded,
    beginGovernanceAudit,
    commitGovernanceAudit,
    refreshWorkflowRuns,
    refreshInvitationLists,
  };
}

export { fetchGovernanceAudit };
