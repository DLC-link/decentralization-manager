import { useCallback, useEffect, useState } from "react";
import { API_BASE } from "../constants";
import { authenticatedFetch } from "../api";
import type { PartyActions } from "../components/NotificationsView";
import type { AuditLogEntry, DecentralizedParty, PartyAuthStatus } from "../types";
import { fetchGovernanceAudit } from "./useNotificationFeeds";
import { useLatestFetch } from "./useLatestFetch";

/// Proposals fetched per request. The feed grows a batch at a time rather
/// than reading a party's whole proposal set at once.
const ACTIONS_BATCH = 25;

interface PartyActionsArgs {
  parties: DecentralizedParty[];
  authStatuses: PartyAuthStatus[];
  /** True until App's first load finishes; the poll waits for it. */
  loading: boolean;
  beginGovernanceAudit: () => () => boolean;
  commitGovernanceAudit: (entries: AuditLogEntry[]) => void;
}

/**
 * The pending governance proposals of every configured party: the first
 * batch, polled every 10 seconds once App has loaded, and the later batches
 * that scrolling pulls in.
 */
export function usePartyActions({
  parties,
  authStatuses,
  loading,
  beginGovernanceAudit,
  commitGovernanceAudit,
}: PartyActionsArgs) {
  const [partyActions, setPartyActions] = useState<PartyActions[]>([]);
  const [partyActionsLoaded, setPartyActionsLoaded] = useState(false);
  const [partyActionsFetching, setPartyActionsFetching] = useState(false);
  // Per-party resume token from the last batch. A null value means that party
  // has no more proposals; a missing key means we never asked.
  const [actionCursors, setActionCursors] = useState<
    Record<string, string | null>
  >({});
  const [loadingMoreActions, setLoadingMoreActions] = useState(false);
  // True once a later batch has been pulled in. The poll replaces state
  // wholesale, so it must not run while a queue is being read or it would
  // discard everything scrolled and snap back to the first batch.
  const [actionsAccumulated, setActionsAccumulated] = useState(false);
  // `performance.now()` when the committed `partyActions` was requested.
  // Completed compares it with when each audit row was first seen.
  const [pendingRequestedAt, setPendingRequestedAt] = useState(
    Number.NEGATIVE_INFINITY,
  );
  const beginPartyActions = useLatestFetch();

  const isGovRulesTemplate = (templateId: string) =>
    templateId === "Governance.Rules:GovernanceRules";

  /** Read the first batch of every configured party's pending proposals. */
  const fetchPartyActions = useCallback(async (): Promise<PartyActions[]> => {
    const candidates = parties
      .map((p) => {
        const authStatus = authStatuses.find(
          (a) => a.dec_party_id === p.party_id,
        );
        const rulesContract = p.contracts?.find((c) =>
          isGovRulesTemplate(c.template_id),
        );
        if (
          !authStatus ||
          authStatus.status.status !== "authenticated" ||
          !authStatus.rights?.dec_party_act_as ||
          !rulesContract
        ) {
          return null;
        }
        const governanceType = "core_self" as const;
        return { party: p, authStatus, rulesContract, governanceType };
      })
      .filter((c): c is NonNullable<typeof c> => c !== null);

    const results = await Promise.all(
      candidates.map(
        async ({
          party,
          authStatus,
          rulesContract,
          governanceType,
        }): Promise<PartyActions | null> => {
          try {
            const res = await authenticatedFetch(
              `${API_BASE}/governance/confirmations?party_id=${encodeURIComponent(party.party_id)}&limit=${ACTIONS_BATCH}`,
            );
            if (!res.ok) return null;
            const data = await res.json();
            return {
              partyId: party.party_id,
              nextCursor: (data.next_cursor ?? null) as string | null,
              // Prefer the live rules contract id from the API — confirm
              // archives + re-creates the rules contract, so the cached
              // `parties` snapshot can point at an archived contract id.
              rulesContractId: data.rules_contract_id ?? rulesContract.contract_id,
              memberPartyId: data.member_party_id ?? authStatus.member_party_id,
              governanceType,
              threshold: data.threshold,
              actions: data.actions ?? [],
              domainActions: data.domain_actions ?? [],
            };
          } catch {
            return null;
          }
        },
      ),
    );
    return results.filter((r): r is PartyActions => r !== null);
  }, [parties, authStatuses]);

  const commitPartyActions = useCallback(
    (kept: PartyActions[], requestedAt: number) => {
      setPartyActions(kept);
      setPendingRequestedAt(requestedAt);
      setActionCursors(
        Object.fromEntries(
          kept.map((p) => [p.partyId, p.nextCursor ?? null]),
        ),
      );
      setActionsAccumulated(false);
      setPartyActionsLoaded(true);
      setPartyActionsFetching(false);
    },
    [],
  );

  const refreshPartyActions = useCallback(async () => {
    // Newest request wins, so a slow poll cannot replace a later snapshot.
    const isLatest = beginPartyActions();
    setPartyActionsFetching(true);
    const requestedAt = performance.now();
    const kept = await fetchPartyActions();
    if (!isLatest()) return;
    commitPartyActions(kept, requestedAt);
  }, [beginPartyActions, fetchPartyActions, commitPartyActions]);

  /**
   * Pull the next batch for every party that still has one and append it.
   *
   * Appending rather than replacing is the point: the feed grows as you scroll
   * instead of re-reading a party's whole proposal set on every render.
   */
  const loadMoreActions = useCallback(async () => {
    const pending = Object.entries(actionCursors).filter(
      ([, cursor]) => cursor,
    );
    if (pending.length === 0 || loadingMoreActions) return;
    setLoadingMoreActions(true);
    setActionsAccumulated(true);
    try {
      const batches = await Promise.all(
        pending.map(async ([partyId, cursor]) => {
          try {
            const res = await authenticatedFetch(
              `${API_BASE}/governance/confirmations?party_id=${encodeURIComponent(partyId)}` +
                `&limit=${ACTIONS_BATCH}&cursor=${encodeURIComponent(cursor as string)}`,
            );
            if (!res.ok) return null;
            const data = await res.json();
            return {
              partyId,
              actions: data.actions ?? [],
              domainActions: data.domain_actions ?? [],
              nextCursor: (data.next_cursor ?? null) as string | null,
            };
          } catch {
            return null;
          }
        }),
      );

      setPartyActions((current) =>
        current.map((entry) => {
          const batch = batches.find((b) => b && b.partyId === entry.partyId);
          if (!batch) return entry;
          // De-dupe by cid: a re-issued cursor must not double a card.
          const seen = new Set(
            entry.domainActions.map((d) => d.proposal_cid),
          );
          return {
            ...entry,
            // Kept current so the feed can tell when a party's proposals are
            // all loaded.
            nextCursor: batch.nextCursor,
            domainActions: [
              ...entry.domainActions,
              ...batch.domainActions.filter(
                (d: { proposal_cid: string }) => !seen.has(d.proposal_cid),
              ),
            ],
          };
        }),
      );

      setActionCursors((current) => {
        const next = { ...current };
        for (const batch of batches) {
          if (batch) next[batch.partyId] = batch.nextCursor;
        }
        return next;
      });
    } finally {
      setLoadingMoreActions(false);
    }
  }, [actionCursors, loadingMoreActions]);

  const hasMoreActions = Object.values(actionCursors).some(Boolean);

  // A confirm, execute or cancel changes what is pending and what is logged.
  // Both are read together and committed in one tick, so React renders the
  // pair at once. Read apart, a fresh pending feed could land first and turn
  // an execute this page just made into "You confirmed" for a moment.
  const refreshActionsAndAudit = useCallback(async () => {
    const actionsLatest = beginPartyActions();
    const auditLatest = beginGovernanceAudit();
    setPartyActionsFetching(true);
    const requestedAt = performance.now();
    const [kept, entries] = await Promise.all([
      fetchPartyActions(),
      fetchGovernanceAudit(),
    ]);
    if (entries !== null && auditLatest()) {
      commitGovernanceAudit(entries);
    }
    if (actionsLatest()) {
      commitPartyActions(kept, requestedAt);
    }
  }, [
    beginPartyActions,
    beginGovernanceAudit,
    fetchPartyActions,
    commitGovernanceAudit,
    commitPartyActions,
  ]);

  // Poll governance actions across configured parties (only after parties load)
  useEffect(() => {
    if (loading) return;
    if (actionsAccumulated) return;
    refreshPartyActions();
    const interval = window.setInterval(refreshPartyActions, 10_000);
    return () => clearInterval(interval);
  }, [refreshPartyActions, loading, actionsAccumulated]);

  return {
    partyActions,
    partyActionsLoaded,
    partyActionsFetching,
    loadingMoreActions,
    actionsAccumulated,
    pendingRequestedAt,
    loadMoreActions,
    hasMoreActions,
    refreshActionsAndAudit,
  };
}
