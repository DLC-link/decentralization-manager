import { useCallback, useEffect, useMemo, useRef, useState, type RefObject } from "react";
import { API_BASE } from "../constants";
import { authenticatedFetch } from "../api";
import { TAB_HASHES, buildHash, parseHash, sharedWithSlug } from "../hashRoute";
import type { DecentralizedPartiesResponse, DecentralizedParty, FormerParty } from "../types";

// Saved in index.html <script> before any modules load.
// Strip Keycloak OAuth params that get appended to the hash during check-sso.
const SAVED_HASH = (
  (window as { __INITIAL_HASH__?: string }).__INITIAL_HASH__ ?? ""
).replace(/[&?#](state|session_state|iss|code)=.*/i, "");
const INITIAL_ROUTE = parseHash(SAVED_HASH);

/**
 * The shared scope a party's history entry was opened in. A party's URL is
 * the same in every scope, so `navigate` keeps the scope in the entry's state.
 */
function historyScope(state: unknown): string | null {
  return (state as { sharedWith?: string | null } | null)?.sharedWith ?? null;
}

interface AppRouteArgs {
  parties: DecentralizedParty[];
  formerParties: FormerParty[];
  formerPartiesLoaded: boolean;
  loading: boolean;
  loadParties: (prefix: string, force?: boolean) => Promise<void>;
  packagesPartyId: string | null;
  setActiveTab: (tab: number) => void;
  setSelectedPartyId: (partyId: string | null) => void;
  setSharedWith: (participantId: string | null) => void;
  setPackagesPartyId: (partyId: string | null) => void;
  setConfigPeerId: (participantId: string | null) => void;
  setPartiesView: (view: "decentralized" | "external") => void;
  setPartyFilter: (prefix: string) => void;
  /** Set once the parties list has been read, so the tab does not load it again. */
  partiesLoadedRef: RefObject<boolean>;
  /** The prefix the loaded list was read with, or null before the first read. */
  listPrefixRef: RefObject<string | null>;
}

/**
 * The hash routes of App: `navigate` and the shared view, the party a
 * deep link names, and the browser's Back and Forward. The route state
 * itself stays in App, because the hooks above this one read it.
 */
export function useAppRoute({
  parties,
  formerParties,
  formerPartiesLoaded,
  loading,
  loadParties,
  packagesPartyId,
  setActiveTab,
  setSelectedPartyId,
  setSharedWith,
  setPackagesPartyId,
  setConfigPeerId,
  setPartiesView,
  setPartyFilter,
  partiesLoadedRef,
  listPrefixRef,
}: AppRouteArgs) {
  // The party the packages tab was opened for, when the parties list does not
  // hold it: a deep link loads the packages tab without reading any parties.
  const [fetchedPackagesParty, setFetchedPackagesParty] =
    useState<DecentralizedParty | null>(null);
  const pendingSlug = useRef<string | null>(INITIAL_ROUTE.partySlug);

  const navigate = useCallback(
    (tab: number, partySlug?: string | null, sharedScope: string | null = null) => {
      setActiveTab(tab);
      if (tab !== 0) setSelectedPartyId(null);
      // Only a parties-tab slug keeps the shared filter: the shared list
      // itself, or a party opened from it.
      if (tab !== 0 || !partySlug) setSharedWith(null);
      setPackagesPartyId(tab === 1 ? (partySlug ?? null) : null);
      setConfigPeerId(tab === 2 ? (partySlug ?? null) : null);
      window.history.pushState(
        { sharedWith: sharedScope },
        "",
        buildHash(tab, partySlug),
      );
    },
    [setActiveTab, setSelectedPartyId, setSharedWith, setPackagesPartyId, setConfigPeerId],
  );

  // Show the parties `participantId` also hosts. The list is read again
  // without a prefix, because a prefix would hide some of them.
  const enterSharedView = useCallback(
    (participantId: string) => {
      setSelectedPartyId(null);
      setPartiesView("decentralized");
      setSharedWith(participantId);
      setPartyFilter("");
      partiesLoadedRef.current = true;
      void loadParties("");
    },
    [
      loadParties,
      partiesLoadedRef,
      setSelectedPartyId,
      setPartiesView,
      setSharedWith,
      setPartyFilter,
    ],
  );

  const showSharedParties = useCallback(
    (participantId: string) => {
      navigate(0, sharedWithSlug(participantId));
      enterSharedView(participantId);
      window.scrollTo(0, 0);
    },
    [navigate, enterSharedView],
  );

  // Jump to the notifications tab after a successful workflow / governance
  // action so the user lands on the queue showing what they just produced.
  const goToNotifications = useCallback(() => {
    navigate(TAB_HASHES.indexOf("notifications"));
  }, [navigate]);

  const packagesParty = useMemo(() => {
    if (!packagesPartyId) return null;
    const listed = parties.find((p) => p.party_id === packagesPartyId);
    if (listed) return listed;
    return fetchedPackagesParty?.party_id === packagesPartyId
      ? fetchedPackagesParty
      : null;
  }, [packagesPartyId, parties, fetchedPackagesParty]);

  useEffect(() => {
    if (!packagesPartyId || packagesParty) return;
    let cancelled = false;
    const prefix = packagesPartyId.split("::")[0];
    authenticatedFetch(
      `${API_BASE}/decentralized-parties?prefix=${encodeURIComponent(prefix)}`,
    )
      .then((res) => (res.ok ? res.json() : null))
      .then((data: DecentralizedPartiesResponse | null) => {
        if (cancelled) return;
        const found = data?.parties.find((p) => p.party_id === packagesPartyId);
        if (found) setFetchedPackagesParty(found);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [packagesPartyId, packagesParty]);

  // Once parties are loaded, resolve pending slug → exact match opens detail.
  // A former party is known only once its own list has loaded too.
  useEffect(() => {
    if (loading || !pendingSlug.current) return;
    const slug = pendingSlug.current;
    const hosted = parties.some((p) => p.party_id === slug);
    if (!hosted && !formerPartiesLoaded) return;
    pendingSlug.current = null;
    if (hosted || formerParties.some((p) => p.party_id === slug)) {
      setSelectedPartyId(slug);
    }
  }, [loading, parties, formerParties, formerPartiesLoaded, setSelectedPartyId]);

  // Listen for back/forward browser navigation
  useEffect(() => {
    const onPopState = (event: PopStateEvent) => {
      const { tab, partySlug, packagesPartyId, sharedWith, configPeerId } =
        parseHash(window.location.hash);
      setActiveTab(tab);
      setPackagesPartyId(packagesPartyId);
      setConfigPeerId(configPeerId);
      if (tab === 0) {
        if (sharedWith) {
          enterSharedView(sharedWith);
          return;
        }
        const exactMatch =
          parties.some((p) => p.party_id === partySlug) ||
          formerParties.some((p) => p.party_id === partySlug);
        if (exactMatch) {
          setSelectedPartyId(partySlug);
          setSharedWith(historyScope(event.state));
        } else {
          setSelectedPartyId(null);
          setSharedWith(null);
          const prefix =
            partySlug && !partySlug.includes("::") ? partySlug : "";
          setPartyFilter(prefix);
          // A shared view reads the whole list, so the loaded list can belong
          // to another query than this entry's.
          if (listPrefixRef.current !== null && listPrefixRef.current !== prefix) {
            void loadParties(prefix);
          }
        }
      } else {
        setSelectedPartyId(null);
        setSharedWith(null);
      }
    };
    window.addEventListener("popstate", onPopState);
    return () => window.removeEventListener("popstate", onPopState);
  }, [
    parties,
    formerParties,
    enterSharedView,
    loadParties,
    listPrefixRef,
    setActiveTab,
    setPackagesPartyId,
    setConfigPeerId,
    setSelectedPartyId,
    setSharedWith,
    setPartyFilter,
  ]);

  return { navigate, showSharedParties, goToNotifications, packagesParty };
}

export { INITIAL_ROUTE };
