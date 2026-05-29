import { useEffect, useMemo, useState, useCallback, useRef } from "react";
import {
  Badge,
  Container,
  Box,
  Alert,
  Fab,
  TextField,
  InputAdornment,
  IconButton,
  LinearProgress,
  Tabs,
  Tab,
  Divider,
  Tooltip,
  useMediaQuery,
  useTheme,
} from "@mui/material";
import FilterListIcon from "@mui/icons-material/FilterList";
import SearchIcon from "@mui/icons-material/Search";
import AddIcon from "@mui/icons-material/Add";
import VisibilityIcon from "@mui/icons-material/Visibility";
import VisibilityOffIcon from "@mui/icons-material/VisibilityOff";
import { Header } from "./components/Header";
import { Sidebar, SIDEBAR_WIDTH } from "./components/Sidebar";
import { PartyList } from "./components/PartyList";
import { PartyDetail } from "./components/PartyDetail";
import { NodeConfigAccordion } from "./components/NodeConfigAccordion";
import { NetworkConfigAccordion } from "./components/NetworkConfigAccordion";
import { PackagesPanel } from "./components/PackagesPanel";
import { LoadingSkeleton, ConfigTabSkeleton } from "./components/LoadingSkeleton";
import { DarsDialog } from "./components/DarsDialog";
import { OnboardingDialog } from "./components/OnboardingDialog";
import { NotificationsView } from "./components/NotificationsView";
import type { PartyActions } from "./components/NotificationsView";
import { useSnackbar } from "./contexts";
import { API_BASE, ADMIN_ACCESS } from "./constants";
import { authenticatedFetch } from "./api";
import { useHiddenParties } from "./useHiddenParties";
import type {
  DecentralizedParty,
  DecentralizedPartiesResponse,
  NodeConfig,
  NetworkConfig,
  ParticipantStatus,
  KeyStatusResponse,
  Peer,
  PendingInvitation,
  PartyAuthStatus,
  AuthStatusResponse,
  WorkflowRun,
} from "./types";

const TAB_HASHES = ["parties", "packages", "config", "notifications"] as const;

// Saved in index.html <script> before any modules load.
// Strip Keycloak OAuth params that get appended to the hash during check-sso.
const SAVED_HASH = (
  (window as { __INITIAL_HASH__?: string }).__INITIAL_HASH__ ?? ""
).replace(/[&?#](state|session_state|iss|code)=.*/i, "");
const INITIAL_ROUTE = parseHash(SAVED_HASH);

function parseHash(hash: string): {
  tab: number;
  partySlug: string | null;
} {
  const raw = hash.replace(/^#\/?/, "");
  const [section, ...rest] = raw.split("/");
  const slug = rest.join("/") || null;

  const tabIndex = TAB_HASHES.indexOf(
    section as (typeof TAB_HASHES)[number],
  );
  return { tab: tabIndex >= 0 ? tabIndex : 0, partySlug: tabIndex === 0 ? slug : null };
}

function buildHash(tab: number, partySlug?: string | null): string {
  const section = TAB_HASHES[tab] ?? "parties";
  return partySlug ? `#${section}/${partySlug}` : `#${section}`;
}

const App = () => {
  const muiTheme = useTheme();
  const isLargeScreen = useMediaQuery(muiTheme.breakpoints.up("lg"));
  const [activeTab, setActiveTab] = useState(INITIAL_ROUTE.tab);
  const [parties, setParties] = useState<DecentralizedParty[]>([]);
  const [nodeConfig, setNodeConfig] = useState<NodeConfig | null>(null);
  const [networkConfig, setNetworkConfig] = useState<NetworkConfig | null>(
    null,
  );
  const [participantStatuses, setParticipantStatuses] = useState<
    ParticipantStatus[]
  >([]);
  const [keyStatus, setKeyStatus] = useState<KeyStatusResponse | null>(null);
  const [authStatuses, setAuthStatuses] = useState<PartyAuthStatus[]>([]);
  const [packageCount, setPackageCount] = useState(0);
  const [onboardingDialogOpen, setOnboardingDialogOpen] = useState(false);
  const [darsDialogOpen, setDarsDialogOpen] = useState(false);
  const [uploadDarsDialogOpen, setUploadDarsDialogOpen] = useState(false);
  const [pendingInvitations, setPendingInvitations] = useState<
    PendingInvitation[]
  >([]);
  const [invitationsLoaded, setInvitationsLoaded] = useState(false);
  const [partyActions, setPartyActions] = useState<PartyActions[]>([]);
  const [partyActionsLoaded, setPartyActionsLoaded] = useState(false);
  const [workflowRuns, setWorkflowRuns] = useState<WorkflowRun[]>([]);
  const [workflowRunsLoaded, setWorkflowRunsLoaded] = useState(false);
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
  const [showSearchBar, setShowSearchBar] = useState(true);
  const [showHidden, setShowHidden] = useState(false);
  const { toggle: toggleHidden, isHidden } = useHiddenParties();
  const visibleParties = useMemo(
    () => (showHidden ? parties : parties.filter((p) => !isHidden(p.party_id))),
    [parties, showHidden, isHidden],
  );
  const hiddenCount = useMemo(
    () => parties.filter((p) => isHidden(p.party_id)).length,
    [parties, isHidden],
  );
  const lastScrollY = useRef(0);
  const savedScrollY = useRef(0);
  const pendingSlug = useRef<string | null>(INITIAL_ROUTE.partySlug);
  const { showSnackbar } = useSnackbar();

  // --- Hash-based routing ---

  const navigate = useCallback(
    (tab: number, partySlug?: string | null) => {
      setActiveTab(tab);
      if (tab !== 0) setSelectedPartyId(null);
      window.history.pushState(null, "", buildHash(tab, partySlug));
    },
    [],
  );

  // Jump to the notifications tab after a successful workflow / governance
  // action so the user lands on the queue showing what they just produced.
  const goToNotifications = useCallback(() => {
    navigate(TAB_HASHES.indexOf("notifications"));
  }, [navigate]);

  // Once parties are loaded, resolve pending slug → exact match opens detail
  useEffect(() => {
    if (loading || !pendingSlug.current) return;
    const slug = pendingSlug.current;
    pendingSlug.current = null;
    const exactMatch = parties.find((p) => p.party_id === slug);
    if (exactMatch) {
      setSelectedPartyId(slug);
    }
  }, [loading, parties]);

  // Listen for back/forward browser navigation
  useEffect(() => {
    const onPopState = () => {
      const { tab, partySlug } = parseHash(window.location.hash);
      setActiveTab(tab);
      if (tab === 0) {
        const exactMatch = parties.find((p) => p.party_id === partySlug);
        if (exactMatch) {
          setSelectedPartyId(partySlug);
        } else {
          setSelectedPartyId(null);
          if (partySlug && !partySlug.includes("::")) {
            setPartyFilter(partySlug);
          }
        }
      } else {
        setSelectedPartyId(null);
      }
    };
    window.addEventListener("popstate", onPopState);
    return () => window.removeEventListener("popstate", onPopState);
  }, [parties]);

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
      setRefreshingParties(true);
      // Update hash to reflect the current filter
      window.history.replaceState(
        null,
        "",
        buildHash(0, partyFilter || null),
      );
      try {
        const qs = new URLSearchParams();
        if (partyFilter) qs.set("prefix", partyFilter);
        if (force) qs.set("refresh", "true");
        const params = qs.toString() ? `?${qs.toString()}` : "";
        const res = await authenticatedFetch(`${API_BASE}/decentralized-parties${params}`);
        if (res.ok) {
          const data: DecentralizedPartiesResponse = await res.json();
          setParties(data.parties);
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
      }
    },
    [showSnackbar, partyFilter],
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
  }, []);

  // Lazy-load parties when switching to parties tab (if not loaded on init)
  const partiesLoaded = useRef(INITIAL_ROUTE.tab === 0);
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

  // Poll participant statuses every 2 seconds
  useEffect(() => {
    const fetchStatuses = async () => {
      try {
        const res = await authenticatedFetch(`${API_BASE}/participants-status`);
        if (res.ok) {
          const data = await res.json();
          setParticipantStatuses(data.statuses);
        }
      } catch {
        // Ignore polling errors
      }
    };

    fetchStatuses();
    const interval = window.setInterval(fetchStatuses, 2000);

    return () => clearInterval(interval);
  }, []);

  const refreshInvitations = useCallback(async () => {
    try {
      const res = await authenticatedFetch(`${API_BASE}/invitations`);
      if (res.ok) {
        const data = await res.json();
        setPendingInvitations(data.invitations);
      }
    } catch {
      // Ignore polling errors
    } finally {
      setInvitationsLoaded(true);
    }
  }, []);

  // Poll pending invitations every 2 seconds
  useEffect(() => {
    refreshInvitations();
    const interval = window.setInterval(refreshInvitations, 2000);
    return () => clearInterval(interval);
  }, [refreshInvitations]);

  const refreshWorkflowRuns = useCallback(async () => {
    try {
      const res = await authenticatedFetch(`${API_BASE}/workflows`);
      if (res.ok) {
        const data = await res.json();
        setWorkflowRuns(data.runs ?? []);
      }
    } catch {
      // Ignore polling errors
    } finally {
      setWorkflowRunsLoaded(true);
    }
  }, []);

  // Poll workflow_runs every 2 seconds, same cadence as invitations.
  useEffect(() => {
    refreshWorkflowRuns();
    const interval = window.setInterval(refreshWorkflowRuns, 2000);
    return () => clearInterval(interval);
  }, [refreshWorkflowRuns]);

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
        const label = `${run.kind} workflow`;
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

  const isGovRulesTemplate = (templateId: string) =>
    templateId.includes("VaultGovernanceRules") ||
    templateId.includes("VaultGovernance") ||
    templateId === "Governance.Rules:GovernanceRules";

  const refreshPartyActions = useCallback(async () => {
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
        const governanceType =
          rulesContract.template_id === "Governance.Rules:GovernanceRules"
            ? ("core_self" as const)
            : ("vault" as const);
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
              `${API_BASE}/governance/confirmations?party_id=${encodeURIComponent(party.party_id)}`,
            );
            if (!res.ok) return null;
            const data = await res.json();
            return {
              partyId: party.party_id,
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
    setPartyActions(results.filter((r): r is PartyActions => r !== null));
    setPartyActionsLoaded(true);
  }, [parties, authStatuses]);

  // Poll governance actions across configured parties (only after parties load)
  useEffect(() => {
    if (loading) return;
    refreshPartyActions();
    const interval = window.setInterval(refreshPartyActions, 10_000);
    return () => clearInterval(interval);
  }, [refreshPartyActions, loading]);

  const notificationCount =
    pendingInvitations.length +
    partyActions.reduce(
      (sum, p) => sum + p.actions.length + p.domainActions.length,
      0,
    ) +
    workflowRuns.length;

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
        />
      ) : (
        <Header />
      )}

      {isLargeScreen && activeTab === 0 && !selectedPartyId && !error && (
        <Box
          sx={{
            position: "fixed",
            top: 16,
            left: `${SIDEBAR_WIDTH}px`,
            right: 0,
            zIndex: 1050,
            display: "flex",
            justifyContent: "center",
            opacity: showSearchBar ? 1 : 0,
            transform: showSearchBar
              ? "translateY(0)"
              : "translateY(-20px)",
            transition: "opacity 0.3s ease, transform 0.3s ease",
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
                  refreshParties();
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
              onClick={() => refreshParties()}
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
          ...(isLargeScreen && { ml: `${SIDEBAR_WIDTH}px` }),
          ...(activeTab === 1 && {
            height: "100vh",
            overflow: "hidden",
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
                  Pending approvals
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
          ...((activeTab === 0 || activeTab === 2 || (isLargeScreen && activeTab === 1)) && { display: "none" }),
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
        <Box sx={{ pt: isLargeScreen ? 4 : 0 }}>
          {selectedPartyId && parties.find((p) => p.party_id === selectedPartyId) ? (
            <PartyDetail
              party={parties.find((p) => p.party_id === selectedPartyId)!}
              onBack={() => {
                setSelectedPartyId(null);
                navigate(0, partyFilter || null);
                window.scrollTo(0, savedScrollY.current);
              }}
              onRefresh={() => refreshParties(true)}
              onNavigateToNotifications={goToNotifications}
              selfParticipantId={nodeConfig?.node.participant_id}
              authStatus={authStatuses.find(
                (a) => a.dec_party_id === selectedPartyId,
              )}
              onAuthRefresh={refreshAuthStatus}
              operatorParty={operatorParty}
              network={nodeConfig?.canton.network}
            />
          ) : (
            <>
              {isLargeScreen ? (
                <Box sx={{ height: 48 }} />
              ) : (
                <Box sx={{ mt: 2, mb: 2, px: 2 }}>
                  <Box
                    sx={{
                      display: "flex",
                      alignItems: "flex-start",
                      gap: 1,
                    }}
                  >
                    <TextField
                      size="small"
                      placeholder="Filter by full prefix (e.g. cbtc-network)"
                      value={partyFilter}
                      onChange={(e) => setPartyFilter(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          refreshParties();
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
                      sx={{ width: 300 }}
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
                          sx={{ mt: "1px" }}
                        >
                          {showHidden ? <VisibilityIcon /> : <VisibilityOffIcon />}
                        </IconButton>
                      </span>
                    </Tooltip>
                    <IconButton
                      onClick={() => refreshParties()}
                      disabled={refreshingParties}
                      color="primary"
                      sx={{ mt: "1px" }}
                    >
                      <SearchIcon />
                    </IconButton>
                  </Box>
                  {refreshingParties && (
                    <LinearProgress sx={{ mt: 1, borderRadius: 1 }} />
                  )}
                </Box>
              )}

              <PartyList
                parties={visibleParties}
                authStatuses={authStatuses}
                onSelectParty={(id) => {
                  savedScrollY.current = window.scrollY;
                  setSelectedPartyId(id);
                  navigate(0, id);
                  window.scrollTo(0, 0);
                }}
                isHidden={isHidden}
                onToggleHidden={toggleHidden}
              />
            </>
          )}

          {ADMIN_ACCESS && !selectedPartyId && (
            <Tooltip title="Create Party" arrow>
              <Fab
                color="primary"
                onClick={() => setOnboardingDialogOpen(true)}
                sx={{
                  position: "fixed",
                  bottom: 24,
                  right: 24,
                  zIndex: 1101,
                }}
              >
                <AddIcon />
              </Fab>
            </Tooltip>
          )}
        </Box>
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
          />
        </Box>
      )}

      {/* Tab 2: Configuration — edge-to-edge */}
      {activeTab === 2 && !loading && !error && (
        <Box sx={{ pt: isLargeScreen ? 4 : 0 }}>
          {nodeConfig ? (
            <>
              <Box sx={{ px: 3, py: 2 }}>
                <NodeConfigAccordion config={nodeConfig} />
              </Box>
              <Divider />
              {networkConfig ? (
                <NetworkConfigAccordion
                  config={networkConfig}
                  nodeConfig={nodeConfig ?? undefined}
                  keyStatus={keyStatus ?? undefined}
                  participantStatuses={participantStatuses}
                  onSave={savePeers}
                />
              ) : (
                <Box sx={{ p: 3 }}>
                  <ConfigTabSkeleton />
                </Box>
              )}
            </>
          ) : (
            <Box sx={{ p: 3 }}>
              <ConfigTabSkeleton />
            </Box>
          )}
        </Box>
      )}

      {/* Tab 3: Notifications */}
      {activeTab === 3 && !loading && !error && (
        <Box sx={{ pt: isLargeScreen ? 4 : 0 }}>
          <NotificationsView
            pendingInvitations={pendingInvitations}
            partyActions={partyActions}
            workflowRuns={workflowRuns}
            loading={
              !invitationsLoaded || !partyActionsLoaded || !workflowRunsLoaded
            }
            onInvitationsChanged={refreshInvitations}
            onActionsChanged={refreshPartyActions}
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
