import { useEffect, useMemo, useState, useCallback, useRef } from "react";
import {
  Badge,
  Container,
  Box,
  Alert,
  TextField,
  InputAdornment,
  IconButton,
  Tabs,
  Tab,
  Tooltip,
  useMediaQuery,
  useTheme,
} from "@mui/material";
import FilterListIcon from "@mui/icons-material/FilterList";
import SearchIcon from "@mui/icons-material/Search";
import VisibilityIcon from "@mui/icons-material/Visibility";
import VisibilityOffIcon from "@mui/icons-material/VisibilityOff";
import { Header } from "./components/Header";
import { Sidebar, SIDEBAR_WIDTH, SIDEBAR_WIDTH_COLLAPSED } from "./components/Sidebar";
import { useNodeHealth } from "./useNodeHealth";
import { PackagesPanel } from "./components/PackagesPanel";
import { LoadingSkeleton } from "./components/LoadingSkeleton";
import { DarsDialog } from "./components/DarsDialog";
import { PartiesTab } from "./components/PartiesTab";
import { ConfigurationTab } from "./components/ConfigurationTab";
import type { PartyFilterControls } from "./components/partyFilterControls";
import { OnboardingDialog } from "./components/OnboardingDialog";
import { useExternalParties } from "./useExternalParties";
import { NotificationsView } from "./components/NotificationsView";
import { useSnackbar } from "./contexts";
import { API_BASE } from "./constants";
import { authenticatedFetch } from "./api";
import { useHiddenParties } from "./useHiddenParties";
import { favoritesFirst, useFavoriteParties } from "./useFavoriteParties";
import { useFormerParties } from "./useFormerParties";
import { INITIAL_ROUTE, useAppRoute } from "./hooks/useAppRoute";
import { useNotificationFeeds } from "./hooks/useNotificationFeeds";
import { usePartyActions } from "./hooks/usePartyActions";
import { useParticipantStatuses } from "./hooks/useParticipantStatuses";
import { buildHash, sharedWithSlug } from "./hashRoute";
import { partiesSharedWith } from "./sharedParties";
import type {
  DecentralizedParty,
  DecentralizedPartiesResponse,
  NodeConfig,
  NetworkConfig,
  KeyStatusResponse,
  Peer,
  PartyAuthStatus,
  AuthStatusResponse,
} from "./types";

const App = () => {
  const muiTheme = useTheme();
  const isLargeScreen = useMediaQuery(muiTheme.breakpoints.up("lg"));
  const [sidebarCollapsed, setSidebarCollapsed] = useState(
    () => localStorage.getItem("sidebar-collapsed") === "true",
  );
  useEffect(() => {
    localStorage.setItem("sidebar-collapsed", String(sidebarCollapsed));
  }, [sidebarCollapsed]);
  const sidebarWidth = sidebarCollapsed ? SIDEBAR_WIDTH_COLLAPSED : SIDEBAR_WIDTH;
  const [activeTab, setActiveTab] = useState(INITIAL_ROUTE.tab);
  const [parties, setParties] = useState<DecentralizedParty[]>([]);
  // The prefix `parties` was last loaded with, so the former parties can be
  // narrowed to match.
  const [loadedPrefix, setLoadedPrefix] = useState("");
  const {
    formerParties,
    loaded: formerPartiesLoaded,
    reload: reloadFormerParties,
    markPurging,
  } = useFormerParties();
  const [partiesView, setPartiesView] = useState<"decentralized" | "external">(
    "decentralized",
  );
  const externalParties = useExternalParties(
    activeTab === 0 && partiesView === "external",
  );
  const [nodeConfig, setNodeConfig] = useState<NodeConfig | null>(null);
  const [networkConfig, setNetworkConfig] = useState<NetworkConfig | null>(
    null,
  );
  const [keyStatus, setKeyStatus] = useState<KeyStatusResponse | null>(null);
  const [authStatuses, setAuthStatuses] = useState<PartyAuthStatus[]>([]);
  const [packageCount, setPackageCount] = useState(0);
  const [onboardingDialogOpen, setOnboardingDialogOpen] = useState(false);
  const [darsDialogOpen, setDarsDialogOpen] = useState(false);
  const [uploadDarsDialogOpen, setUploadDarsDialogOpen] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [partyFilter, setPartyFilter] = useState(
    INITIAL_ROUTE.partySlug && !INITIAL_ROUTE.partySlug.includes("::")
      ? INITIAL_ROUTE.partySlug
      : "",
  );
  const [refreshingParties, setRefreshingParties] = useState(false);
  const [packagesRefreshNonce, setPackagesRefreshNonce] = useState(0);
  const [operatorParty, setOperatorParty] = useState("");
  const [selectedPartyId, setSelectedPartyId] = useState<string | null>(null);
  const [packagesPartyId, setPackagesPartyId] = useState<string | null>(
    INITIAL_ROUTE.packagesPartyId,
  );
  // The participant whose shared parties the list shows. It narrows the list
  // on top of the prefix, and stays set while a party opened from it is shown,
  // so Back returns to the same list.
  const [sharedWith, setSharedWith] = useState<string | null>(
    INITIAL_ROUTE.sharedWith,
  );
  const [configPeerId, setConfigPeerId] = useState<string | null>(
    INITIAL_ROUTE.configPeerId,
  );
  const [showSearchBar, setShowSearchBar] = useState(true);
  const [showHidden, setShowHidden] = useState(false);
  const { toggle: toggleHidden, isHidden } = useHiddenParties();
  const { favorites, toggle: toggleFavorite, isFavorite } = useFavoriteParties();
  // Only the Configuration tab shows node health, and only it pays for the
  // poll: the probe stops the moment the operator moves to another tab.
  const nodeHealth = useNodeHealth(activeTab === 2);
  const listedParties = useMemo(
    () => (sharedWith ? partiesSharedWith(parties, sharedWith) : parties),
    [parties, sharedWith],
  );
  // Starred parties first, before the list paginates, so they land on page one.
  const visibleParties = useMemo(
    () =>
      favoritesFirst(
        showHidden
          ? listedParties
          : listedParties.filter((p) => !isHidden(p.party_id)),
        favorites,
      ),
    [listedParties, showHidden, isHidden, favorites],
  );
  const hiddenCount = useMemo(
    () => listedParties.filter((p) => isHidden(p.party_id)).length,
    [listedParties, isHidden],
  );
  // A party hosted again leaves the former list on its next load; until then
  // the hosted entry wins. The shared view lists hosted parties only.
  const visibleFormerParties = useMemo(() => {
    if (sharedWith) return [];
    const hosted = new Set(parties.map((p) => p.party_id));
    return formerParties.filter(
      (p) => p.party_id.startsWith(loadedPrefix) && !hosted.has(p.party_id),
    );
  }, [formerParties, parties, loadedPrefix, sharedWith]);
  const lastScrollY = useRef(0);
  const savedScrollY = useRef(0);
  const partiesLoaded = useRef(INITIAL_ROUTE.tab === 0);
  // The prefix the loaded list was read with, or null before the first read.
  const listPrefix = useRef<string | null>(null);
  const { showSnackbar } = useSnackbar();

  const loadParties = useCallback(
    async (prefix: string, force = false) => {
      setRefreshingParties(true);
      try {
        const qs = new URLSearchParams();
        if (prefix) qs.set("prefix", prefix);
        if (force) qs.set("refresh", "true");
        const params = qs.toString() ? `?${qs.toString()}` : "";
        const res = await authenticatedFetch(`${API_BASE}/decentralized-parties${params}`);
        if (res.ok) {
          const data: DecentralizedPartiesResponse = await res.json();
          setParties(data.parties);
          listPrefix.current = prefix;
          setLoadedPrefix(prefix);
        } else {
          showSnackbar("Failed to refresh parties", "error");
        }
      } catch (err) {
        showSnackbar(
          err instanceof Error ? err.message : "Failed to refresh parties",
          "error",
        );
      } finally {
        setRefreshingParties(false);
        // A refresh from Canton is what records a removal, so read the former
        // parties after it.
        void reloadFormerParties();
      }
    },
    [showSnackbar, reloadFormerParties],
  );

  // --- Hash-based routing ---

  const { navigate, showSharedParties, goToNotifications, packagesParty } = useAppRoute({
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
    partiesLoadedRef: partiesLoaded,
    listPrefixRef: listPrefix,
  });

  useEffect(() => {
    if ("scrollRestoration" in history) {
      history.scrollRestoration = "manual";
    }
    window.scrollTo(0, 0);
  }, []);

  useEffect(() => {
    const handleScroll = () => {
      const currentScrollY = window.scrollY;
      if (currentScrollY > lastScrollY.current && currentScrollY > 100) {
        setShowSearchBar(false);
      } else {
        setShowSearchBar(true);
      }
      lastScrollY.current = currentScrollY;
    };

    window.addEventListener("scroll", handleScroll, { passive: true });
    return () => window.removeEventListener("scroll", handleScroll);
  }, []);

  useEffect(() => {
    if (!nodeConfig) return;
    authenticatedFetch(`${API_BASE}/operator-info`)
      .then((res) => (res.ok ? res.json() : null))
      .then((data: { party_id: string } | null) => {
        if (data) setOperatorParty(data.party_id);
      })
      .catch(() => {});
  }, [nodeConfig]);

  const refreshParties = useCallback(
    async (force = false) => {
      // Update hash to reflect the current filter
      window.history.replaceState(
        null,
        "",
        buildHash(
          0,
          sharedWith ? sharedWithSlug(sharedWith) : partyFilter || null,
        ),
      );
      await loadParties(partyFilter, force);
    },
    [loadParties, partyFilter, sharedWith],
  );

  const savePeers = useCallback(
    async (peers: Peer[]) => {
      const res = await authenticatedFetch(`${API_BASE}/network-config`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(peers),
      });
      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || "Failed to save peers");
      }
      // Refresh the network config
      const networkRes = await authenticatedFetch(`${API_BASE}/network-config`);
      if (networkRes.ok) {
        const networkData = await networkRes.json();
        setNetworkConfig(networkData);
      }
      showSnackbar("Peers saved successfully");
    },
    [showSnackbar],
  );

  const refreshAuthStatus = useCallback(async () => {
    try {
      const res = await authenticatedFetch(`${API_BASE}/auth/status`);
      if (res.ok) {
        const data: AuthStatusResponse = await res.json();
        setAuthStatuses(data.parties);
      }
    } catch {
      // Ignore auth status errors
    }
  }, []);

  useEffect(() => {
    const fetchData = async () => {
      try {
        // Fetch node config first to check test_mode early
        const nodeRes = await authenticatedFetch(`${API_BASE}/node-config`);
        if (!nodeRes.ok) throw new Error("Failed to fetch node config");
        const nodeData = await nodeRes.json();
        setNodeConfig(nodeData);

        // If on /swagger-ui/ without test mode, skip loading everything else
        if (
          window.location.pathname.startsWith("/swagger-ui") &&
          !nodeData.test_mode
        ) {
          setLoading(false);
          return;
        }

        const { tab: hashTab, partySlug: hashSlug } = INITIAL_ROUTE;

        const fetches: Promise<Response>[] = [
          authenticatedFetch(`${API_BASE}/auth/status`),
          authenticatedFetch(`${API_BASE}/packages/vetted`),
        ];

        // Eagerly fetch parties on the parties tab (we need to render the
        // list) and on the notifications tab (governance actions are derived
        // per-party, so without parties the notifications feed is incomplete).
        if (hashTab === 0 || hashTab === 3) {
          const partiesParams = hashSlug
            ? `?prefix=${encodeURIComponent(hashSlug)}`
            : "";
          fetches.push(
            authenticatedFetch(`${API_BASE}/decentralized-parties${partiesParams}`),
          );
        }

        const [authStatusRes, packagesRes, partiesRes] =
          await Promise.all(fetches);

        if (partiesRes) {
          if (!partiesRes.ok) {
            throw new Error("Failed to fetch data");
          }
          const partiesData: DecentralizedPartiesResponse =
            await partiesRes.json();
          setParties(partiesData.parties);
          listPrefix.current = hashSlug ?? "";
          setLoadedPrefix(hashSlug ?? "");
          void reloadFormerParties();
        }

        if (authStatusRes.ok) {
          const authStatusData: AuthStatusResponse = await authStatusRes.json();
          setAuthStatuses(authStatusData.parties);
        }

        if (packagesRes.ok) {
          const packagesData = await packagesRes.json();
          setPackageCount(Array.isArray(packagesData) ? packagesData.length : 0);
        }
      } catch (err) {
        setError(err instanceof Error ? err.message : "Unknown error");
      } finally {
        setLoading(false);
      }
    };

    fetchData();
  }, [reloadFormerParties]);

  // Lazy-load parties when switching to parties tab (if not loaded on init)
  useEffect(() => {
    if (activeTab !== 0 || partiesLoaded.current) return;
    partiesLoaded.current = true;
    refreshParties();
  }, [activeTab, refreshParties]);

  // Lazy-load config tab data when first opened
  useEffect(() => {
    if (activeTab !== 2) return;
    if (networkConfig && keyStatus) return; // already loaded
    const fetchConfigData = async () => {
      try {
        const [networkRes, keyStatusRes] = await Promise.all([
          authenticatedFetch(`${API_BASE}/network-config`),
          authenticatedFetch(`${API_BASE}/keys/status`),
        ]);
        if (networkRes.ok) setNetworkConfig(await networkRes.json());
        if (keyStatusRes.ok) setKeyStatus(await keyStatusRes.json());
      } catch {
        // Ignore — will show empty state
      }
    };
    fetchConfigData();
  }, [activeTab, networkConfig, keyStatus]);

  // The shared view's filter chip names the participant by its peer name.
  useEffect(() => {
    if (!sharedWith || networkConfig) return;
    let cancelled = false;
    authenticatedFetch(`${API_BASE}/network-config`)
      .then((res) => (res.ok ? res.json() : null))
      .then((data: NetworkConfig | null) => {
        if (!cancelled && data) setNetworkConfig(data);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [sharedWith, networkConfig]);

  const { participantStatuses, selfWireVersion, selfLatency } = useParticipantStatuses();
  const {
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
  } = useNotificationFeeds();

  const {
    partyActions,
    partyActionsLoaded,
    partyActionsFetching,
    loadingMoreActions,
    actionsAccumulated,
    pendingRequestedAt,
    loadMoreActions,
    hasMoreActions,
    refreshActionsAndAudit,
  } = usePartyActions({
    parties,
    authStatuses,
    loading,
    beginGovernanceAudit,
    commitGovernanceAudit,
  });

  const notificationCount =
    pendingInvitations.length +
    partyActions.reduce(
      (sum, p) => sum + p.actions.length + p.domainActions.length,
      0,
    ) +
    workflowRuns.length;

  const selectedParty = selectedPartyId
    ? parties.find((p) => p.party_id === selectedPartyId)
    : undefined;
  const selectedFormerParty =
    selectedPartyId && !selectedParty
      ? formerParties.find((p) => p.party_id === selectedPartyId)
      : undefined;
  // A selected party that has left both lists falls back to the list, which
  // then needs its search bar and FAB back.
  const showingDetail = Boolean(selectedParty || selectedFormerParty);

  const filterControls: PartyFilterControls = {
    partyFilter,
    setPartyFilter,
    refreshParties,
    refreshingParties,
    showHidden,
    setShowHidden,
    hiddenCount,
  };

  const backToList = () => {
    setSelectedPartyId(null);
    navigate(0, sharedWith ? sharedWithSlug(sharedWith) : partyFilter || null);
    window.scrollTo(0, savedScrollY.current);
  };

  const buildInfo = nodeConfig
    ? {
        version: nodeConfig.version,
        buildVersion: nodeConfig.build_version,
        buildTime: nodeConfig.build_time,
      }
    : undefined;

  return (
    <Box
      sx={{
        minHeight: "100vh",
        backgroundColor: (theme) => theme.palette.background.default,
      }}
    >
      {isLargeScreen ? (
        <Sidebar
          activeTab={activeTab}
          onTabChange={(tab) => navigate(tab)}
          partyCount={parties.length}
          packageCount={packageCount}
          notificationCount={notificationCount}
          buildInfo={buildInfo}
          collapsed={sidebarCollapsed}
          onToggleCollapsed={() => setSidebarCollapsed((c) => !c)}
          network={nodeConfig?.canton.network}
        />
      ) : (
        <Header buildInfo={buildInfo} />
      )}

      {isLargeScreen && activeTab === 0 && !showingDetail && !error && (
        <Box
          sx={{
            position: "fixed",
            top: 16,
            left: `${sidebarWidth}px`,
            right: 0,
            zIndex: 1050,
            display: "flex",
            justifyContent: "center",
            opacity: showSearchBar ? 1 : 0,
            transform: showSearchBar
              ? "translateY(0)"
              : "translateY(-20px)",
            transition: "left 0.15s ease-out, opacity 0.3s ease, transform 0.3s ease",
            pointerEvents: showSearchBar ? "auto" : "none",
          }}
        >
          <Box
            sx={{
              display: "flex",
              alignItems: "center",
              gap: 1,
              backdropFilter: "blur(16px)",
              backgroundColor: (theme) =>
                theme.palette.mode === "light"
                  ? "rgba(255, 255, 255, 0.85)"
                  : "rgba(42, 42, 42, 0.85)",
              borderRadius: 100,
              pl: 0.5,
              pr: 0.5,
              py: 0.5,
              boxShadow: (theme) =>
                theme.palette.mode === "light"
                  ? "0 2px 12px rgba(0,0,0,0.08)"
                  : "0 2px 12px rgba(0,0,0,0.3)",
              border: (theme) =>
                `1px solid ${theme.palette.mode === "light" ? "rgba(0,0,0,0.08)" : "rgba(255,255,255,0.08)"}`,
            }}
          >
            <TextField
              size="small"
              placeholder="Filter by full prefix (e.g. cbtc-network)"
              value={partyFilter}
              onChange={(e) => setPartyFilter(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  refreshParties(true);
                }
              }}
              disabled={refreshingParties}
              slotProps={{
                input: {
                  startAdornment: (
                    <InputAdornment position="start">
                      <FilterListIcon fontSize="small" color="action" />
                    </InputAdornment>
                  ),
                },
              }}
              sx={{
                width: 380,
                "& .MuiOutlinedInput-root": {
                  borderRadius: 100,
                },
                "& .MuiOutlinedInput-notchedOutline": {
                  border: "none",
                },
              }}
            />
            <Tooltip
              title={
                showHidden
                  ? `Hide ${hiddenCount} hidden ${hiddenCount === 1 ? "party" : "parties"}`
                  : hiddenCount > 0
                    ? `Show ${hiddenCount} hidden ${hiddenCount === 1 ? "party" : "parties"}`
                    : "No hidden parties"
              }
            >
              <span>
                <IconButton
                  onClick={() => setShowHidden((v) => !v)}
                  disabled={hiddenCount === 0 && !showHidden}
                  color={showHidden ? "primary" : "default"}
                >
                  {showHidden ? <VisibilityIcon /> : <VisibilityOffIcon />}
                </IconButton>
              </span>
            </Tooltip>
            <IconButton
              onClick={() => refreshParties(true)}
              disabled={refreshingParties}
              color="primary"
              sx={{
                backgroundColor: "primary.main",
                color: "white",
                "&:hover": {
                  backgroundColor: "primary.dark",
                },
                "&.Mui-disabled": {
                  backgroundColor: "action.disabledBackground",
                  color: "action.disabled",
                },
              }}
            >
              <SearchIcon />
            </IconButton>
          </Box>
        </Box>
      )}

      <Box
        sx={{
          ...(isLargeScreen && { ml: `${sidebarWidth}px`, transition: "margin-left 0.15s ease-out" }),
          ...(activeTab === 1 && {
            height: "100vh",
            overflow: "hidden",
            display: "flex",
            flexDirection: "column",
          }),
          // The parties list and the approvals feed both end in a footer bar
          // that should sit at the bottom of the view even when a short page
          // doesn't fill it — so the region is at least a screen tall and the
          // list flexes into the leftover.
          ...((activeTab === 0 || activeTab === 3) && {
            minHeight: "100vh",
            display: "flex",
            flexDirection: "column",
          }),
        }}
      >
      {loading && (
        <Box sx={{ pt: isLargeScreen ? 4 : 16 }}>
          {isLargeScreen && <Box sx={{ height: 48 }} />}
          <LoadingSkeleton />
        </Box>
      )}

      {!loading && error && (
        <Container maxWidth="md" sx={{ pt: isLargeScreen ? 4 : 16 }}>
          <Alert severity="error" onClose={() => setError(null)}>
            {error}
          </Alert>
        </Container>
      )}

      {!isLargeScreen && !loading && !error && (
        <Box sx={{ pt: 16, px: 2 }}>
          <Tabs
            value={activeTab}
            onChange={(_e, v) => navigate(v)}
            sx={{
              mb: 1,
              borderBottom: 1,
              borderColor: "divider",
              overflow: "visible",
              "& .MuiTabs-scroller": { overflow: "visible !important" },
              "& .MuiTab-root": { overflow: "visible" },
            }}
          >
            <Tab
              label={
                <Badge badgeContent={parties.length} color="primary" sx={{ pr: parties.length ? 2.5 : 0 }}>
                  Parties
                </Badge>
              }
            />
            <Tab
              label={
                <Badge badgeContent={packageCount} color="primary" sx={{ pr: packageCount ? 2.5 : 0 }}>
                  Packages
                </Badge>
              }
            />
            <Tab label="Configuration" />
            <Tab
              label={
                <Badge
                  badgeContent={notificationCount}
                  color={activeTab === 3 ? "primary" : "error"}
                  sx={{ pr: notificationCount ? 2.5 : 0 }}
                >
                  Approvals
                </Badge>
              }
            />
          </Tabs>
        </Box>
      )}

      <Container
        maxWidth="md"
        sx={{
          pt: isLargeScreen ? 4 : 2,
          pb: 0,
          ...((activeTab === 0 ||
            activeTab === 2 ||
            activeTab === 3 ||
            (isLargeScreen && activeTab === 1)) && { display: "none" }),
        }}
      >
        {window.location.pathname.startsWith("/swagger-ui") &&
        nodeConfig &&
        !nodeConfig.test_mode ? (
          <Alert severity="warning" sx={{ mt: 2 }}>
            Swagger UI is disabled. Restart the server with{" "}
            <strong>serve --test</strong> to enable Swagger UI and mock
            authentication.
          </Alert>
        ) : loading ? (
          <LoadingSkeleton />
        ) : error ? (
          <Alert severity="error" onClose={() => setError(null)}>
            {error}
          </Alert>
        ) : (
          <>
            {/* Tab 0 & 1: rendered outside Container below */}

            {/* Tab 2: rendered outside Container below */}

            <DarsDialog
              open={darsDialogOpen}
              onClose={() => setDarsDialogOpen(false)}
              onComplete={() => {
                refreshParties(true);
                setPackagesRefreshNonce((n) => n + 1);
                goToNotifications();
              }}
              mode="distribute"
            />

            <DarsDialog
              open={uploadDarsDialogOpen}
              onClose={() => setUploadDarsDialogOpen(false)}
              onComplete={() => {
                refreshParties(true);
                setPackagesRefreshNonce((n) => n + 1);
              }}
              mode="upload"
            />

            <OnboardingDialog
              open={onboardingDialogOpen}
              onClose={() => setOnboardingDialogOpen(false)}
              onComplete={() => {
                refreshParties(true);
                goToNotifications();
              }}
            />
          </>
        )}
      </Container>

      {/* Tab 0: Parties — edge-to-edge */}
      {activeTab === 0 && !loading && !error && (
        <PartiesTab
          filter={filterControls}
          isLargeScreen={isLargeScreen}
          selectedParty={selectedParty}
          selectedFormerParty={selectedFormerParty}
          selectedPartyId={selectedPartyId}
          setSelectedPartyId={setSelectedPartyId}
          showingDetail={showingDetail}
          backToList={backToList}
          navigate={navigate}
          goToNotifications={goToNotifications}
          showSharedParties={showSharedParties}
          reloadFormerParties={reloadFormerParties}
          markPurging={markPurging}
          nodeConfig={nodeConfig}
          networkConfig={networkConfig}
          authStatuses={authStatuses}
          refreshAuthStatus={refreshAuthStatus}
          operatorParty={operatorParty}
          partiesView={partiesView}
          setPartiesView={setPartiesView}
          sharedWith={sharedWith}
          setSharedWith={setSharedWith}
          visibleParties={visibleParties}
          visibleFormerParties={visibleFormerParties}
          externalParties={externalParties}
          isHidden={isHidden}
          toggleHidden={toggleHidden}
          isFavorite={isFavorite}
          toggleFavorite={toggleFavorite}
          savedScrollYRef={savedScrollY}
          setOnboardingDialogOpen={setOnboardingDialogOpen}
        />
      )}

      {/* Tab 1: Package Management — edge-to-edge */}
      {activeTab === 1 && !loading && !error && (
        <Box
          sx={{
            display: "flex",
            flexDirection: "column",
            flex: 1,
            minHeight: 0,
          }}
        >
          <PackagesPanel
            onUploadDars={() => setUploadDarsDialogOpen(true)}
            onDistributeDars={() => setDarsDialogOpen(true)}
            refreshNonce={packagesRefreshNonce}
            selfParticipantId={nodeConfig?.node.participant_id}
            party={packagesParty}
            onClearParty={() => navigate(1)}
          />
        </Box>
      )}

      {/* Tab 2: Configuration — edge-to-edge */}
      {activeTab === 2 && !loading && !error && (
        <ConfigurationTab
          isLargeScreen={isLargeScreen}
          nodeConfig={nodeConfig}
          nodeHealth={nodeHealth}
          selfLatency={selfLatency}
          networkConfig={networkConfig}
          keyStatus={keyStatus}
          participantStatuses={participantStatuses}
          selfWireVersion={selfWireVersion}
          savePeers={savePeers}
          configPeerId={configPeerId}
          showSharedParties={showSharedParties}
        />
      )}

      {/* Tab 3: Notifications */}
      {activeTab === 3 && !loading && !error && (
        // Same as the parties tab: passes the leftover height down so the
        // Completed footer bar lands at the bottom of the view. No top padding
        // — this tab opens on a sticky filter bar, which belongs at the top.
        <Box
          sx={{
            flex: 1,
            display: "flex",
            flexDirection: "column",
          }}
        >
          <NotificationsView
            pendingInvitations={pendingInvitations}
            partyActions={partyActions}
            workflowRuns={workflowRuns}
            governanceAudit={governanceAudit.entries}
            auditFirstSeen={governanceAudit.firstSeen}
            pendingRequestedAt={pendingRequestedAt}
            handledInvitations={handledInvitations}
            pendingLive={!actionsAccumulated}
            fetching={partyActionsFetching}
            onLoadMore={loadMoreActions}
            hasMore={hasMoreActions}
            loadingMore={loadingMoreActions}
            loading={
              !invitationsLoaded ||
              !partyActionsLoaded ||
              !workflowRunsLoaded ||
              !governanceAuditLoaded ||
              !handledInvitationsLoaded
            }
            onInvitationsChanged={refreshInvitationLists}
            onActionsChanged={refreshActionsAndAudit}
            onWorkflowsChanged={refreshWorkflowRuns}
            onSelectParty={(partyId) => {
              setSelectedPartyId(partyId);
              navigate(0, partyId);
            }}
          />
        </Box>
      )}
      </Box>
    </Box>
  );
};

export default App;
