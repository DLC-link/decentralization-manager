import { useEffect, useState, useCallback, useRef } from "react";
import {
  Badge,
  Container,
  Box,
  Alert,
  Fab,
  TextField,
  InputAdornment,
  LinearProgress,
  IconButton,
  Tabs,
  Tab,
  Card,
  Divider,
  Tooltip,
  useMediaQuery,
  useTheme,
} from "@mui/material";
import FilterListIcon from "@mui/icons-material/FilterList";
import SearchIcon from "@mui/icons-material/Search";
import AddIcon from "@mui/icons-material/Add";
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
import { InvitationModal } from "./components/InvitationModal";
import { useSnackbar } from "./contexts";
import { API_BASE, ADMIN_ACCESS, OPERATOR_API_URLS } from "./constants";
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
} from "./types";

const App = () => {
  const muiTheme = useTheme();
  const isLargeScreen = useMediaQuery(muiTheme.breakpoints.up("lg"));
  const [activeTab, setActiveTab] = useState(0);
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
  const [_pendingInvitations, setPendingInvitations] = useState<
    PendingInvitation[]
  >([]);
  const [currentInvitation, setCurrentInvitation] =
    useState<PendingInvitation | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [partyFilter, setPartyFilter] = useState("");
  const [refreshingParties, setRefreshingParties] = useState(false);
  const [operatorParty, setOperatorParty] = useState("");
  const [selectedPartyId, setSelectedPartyId] = useState<string | null>(null);
  const [showSearchBar, setShowSearchBar] = useState(true);
  const lastScrollY = useRef(0);
  const savedScrollY = useRef(0);
  const { showSnackbar } = useSnackbar();

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
    const network = nodeConfig?.canton.network;
    if (!network) return;
    const url = OPERATOR_API_URLS[network];
    fetch(url)
      .then((res) => res.json())
      .then((data: { partyId: string }) => setOperatorParty(data.partyId))
      .catch(() => {});
  }, [nodeConfig]);

  const refreshParties = useCallback(async () => {
    setRefreshingParties(true);
    try {
      const params = partyFilter
        ? `?prefix=${encodeURIComponent(partyFilter)}`
        : "";
      const res = await fetch(`${API_BASE}/decentralized-parties${params}`);
      if (res.ok) {
        const data: DecentralizedPartiesResponse = await res.json();
        setParties(data.parties);
      } else {
        showSnackbar("Failed to refresh parties");
      }
    } catch (err) {
      showSnackbar(
        err instanceof Error ? err.message : "Failed to refresh parties",
      );
    } finally {
      setRefreshingParties(false);
    }
  }, [showSnackbar, partyFilter]);

  const savePeers = useCallback(
    async (peers: Peer[]) => {
      const res = await fetch(`${API_BASE}/network-config`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(peers),
      });
      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || "Failed to save peers");
      }
      // Refresh the network config
      const networkRes = await fetch(`${API_BASE}/network-config`);
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
      const res = await fetch(`${API_BASE}/auth/status`);
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
        const nodeRes = await fetch(`${API_BASE}/node-config`);
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

        const partiesParams = partyFilter
          ? `?prefix=${encodeURIComponent(partyFilter)}`
          : "";
        const [partiesRes, authStatusRes, packagesRes] = await Promise.all([
          fetch(`${API_BASE}/decentralized-parties${partiesParams}`),
          fetch(`${API_BASE}/auth/status`),
          fetch(`${API_BASE}/packages/vetted`),
        ]);

        if (!partiesRes.ok) {
          throw new Error("Failed to fetch data");
        }

        const partiesData: DecentralizedPartiesResponse =
          await partiesRes.json();

        setParties(partiesData.parties);

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
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Lazy-load config tab data when first opened
  useEffect(() => {
    if (activeTab !== 2) return;
    if (networkConfig && keyStatus) return; // already loaded
    const fetchConfigData = async () => {
      try {
        const [networkRes, keyStatusRes] = await Promise.all([
          fetch(`${API_BASE}/network-config`),
          fetch(`${API_BASE}/keys/status`),
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
        const res = await fetch(`${API_BASE}/participants-status`);
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

  // Poll pending invitations every 2 seconds
  useEffect(() => {
    const fetchInvitations = async () => {
      try {
        const res = await fetch(`${API_BASE}/invitations`);
        if (res.ok) {
          const data = await res.json();
          setPendingInvitations(data.invitations);
          // Show modal for first invitation if not already showing one
          if (data.invitations.length > 0 && !currentInvitation) {
            setCurrentInvitation(data.invitations[0]);
          }
        }
      } catch {
        // Ignore polling errors
      }
    };

    fetchInvitations();
    const interval = window.setInterval(fetchInvitations, 2000);

    return () => clearInterval(interval);
  }, [currentInvitation]);

  const handleInvitationAction = useCallback(() => {
    setCurrentInvitation(null);
    // Show next invitation if there are more
    setPendingInvitations((prev) => {
      const remaining = prev.filter((i) => i.id !== currentInvitation?.id);
      if (remaining.length > 0) {
        setTimeout(() => setCurrentInvitation(remaining[0]), 500);
      }
      return remaining;
    });
  }, [currentInvitation]);

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
          onTabChange={setActiveTab}
          partyCount={parties.length}
          packageCount={packageCount}
        />
      ) : (
        <Header />
      )}

      {isLargeScreen && activeTab === 0 && !selectedPartyId && !loading && !error && (
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
              InputProps={{
                startAdornment: (
                  <InputAdornment position="start">
                    <FilterListIcon fontSize="small" color="action" />
                  </InputAdornment>
                ),
              }}
              sx={{
                width: 300,
                "& .MuiOutlinedInput-root": {
                  borderRadius: 100,
                },
                "& .MuiOutlinedInput-notchedOutline": {
                  border: "none",
                },
              }}
            />
            <IconButton
              onClick={refreshParties}
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
      <Container
        maxWidth="md"
        sx={{
          pt: isLargeScreen ? 4 : 16,
          pb: 0,
          ...(isLargeScreen && (activeTab === 0 || activeTab === 1) && { display: "none" }),
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
          <Alert severity="error">{error}</Alert>
        ) : (
          <>
            {!isLargeScreen && (
              <Tabs
                value={activeTab}
                onChange={(_e, v) => setActiveTab(v)}
                sx={{
                  mb: 3,
                  borderBottom: 1,
                  borderColor: "divider",
                  overflow: "visible",
                  "& .MuiTabs-scroller": { overflow: "visible !important" },
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
              </Tabs>
            )}

            {/* Tab 0: Decentralized Parties */}
            {activeTab === 0 && (
              <>
                {selectedPartyId && parties.find((p) => p.party_id === selectedPartyId) ? (
                  <PartyDetail
                    party={parties.find((p) => p.party_id === selectedPartyId)!}
                    onBack={() => {
                      setSelectedPartyId(null);
                      window.scrollTo(0, savedScrollY.current);
                    }}
                    onRefresh={refreshParties}
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
                      <Box sx={{ mb: 3 }}>
                        <Box
                          sx={{
                            display: "flex",
                            alignItems: "flex-start",
                            gap: 1,
                          }}
                        >
                          <TextField
                            size="small"
                            label="Filter by full prefix (e.g. cbtc-network)"
                            placeholder="Enter full party prefix"
                            value={partyFilter}
                            onChange={(e) => setPartyFilter(e.target.value)}
                            onKeyDown={(e) => {
                              if (e.key === "Enter") {
                                refreshParties();
                              }
                            }}
                            disabled={refreshingParties}
                            InputProps={{
                              startAdornment: (
                                <InputAdornment position="start">
                                  <FilterListIcon fontSize="small" color="action" />
                                </InputAdornment>
                              ),
                            }}
                            sx={{ width: 300 }}
                          />
                          <IconButton
                            onClick={refreshParties}
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
                      parties={parties}
                      authStatuses={authStatuses}
                      onSelectParty={(id) => {
                        savedScrollY.current = window.scrollY;
                        setSelectedPartyId(id);
                        window.scrollTo(0, 0);
                      }}
                    />
                  </>
                )}

                {ADMIN_ACCESS && (
                  <Tooltip title="Create Party" arrow>
                    <Fab
                      color="primary"
                      onClick={() => setOnboardingDialogOpen(true)}
                      sx={{
                        position: "fixed",
                        bottom: 24,
                        right: 24,
                      }}
                    >
                      <AddIcon />
                    </Fab>
                  </Tooltip>
                )}
              </>
            )}

            {/* Tab 1: rendered outside Container below */}

            {/* Tab 2: Configuration */}
            {activeTab === 2 && (
              <Card sx={{ borderRadius: 2, overflow: "hidden" }}>
                {nodeConfig ? (
                  <>
                    <Box sx={{ p: 3, pb: 2 }}>
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
              </Card>
            )}

            <DarsDialog
              open={darsDialogOpen}
              onClose={() => setDarsDialogOpen(false)}
              onComplete={refreshParties}
              mode="distribute"
            />

            <DarsDialog
              open={uploadDarsDialogOpen}
              onClose={() => setUploadDarsDialogOpen(false)}
              onComplete={refreshParties}
              mode="upload"
            />

            <OnboardingDialog
              open={onboardingDialogOpen}
              onClose={() => setOnboardingDialogOpen(false)}
              onComplete={refreshParties}
            />

            <InvitationModal
              invitation={currentInvitation}
              onClose={() => setCurrentInvitation(null)}
              onAction={handleInvitationAction}
            />
          </>
        )}
      </Container>

      {/* Tab 0: Parties — edge-to-edge on large screens */}
      {isLargeScreen && activeTab === 0 && !loading && !error && (
        <Box sx={{ pt: 4 }}>
          {selectedPartyId && parties.find((p) => p.party_id === selectedPartyId) ? (
            <PartyDetail
              party={parties.find((p) => p.party_id === selectedPartyId)!}
              onBack={() => {
                setSelectedPartyId(null);
                window.scrollTo(0, savedScrollY.current);
              }}
              onRefresh={refreshParties}
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
              <Box sx={{ height: 48 }} />
              <PartyList
                parties={parties}
                authStatuses={authStatuses}
                onSelectParty={(id) => {
                  savedScrollY.current = window.scrollY;
                  setSelectedPartyId(id);
                  window.scrollTo(0, 0);
                }}
              />
            </>
          )}

          {ADMIN_ACCESS && (
            <Tooltip title="Create Party" arrow>
              <Fab
                color="primary"
                onClick={() => setOnboardingDialogOpen(true)}
                sx={{
                  position: "fixed",
                  bottom: 24,
                  right: 24,
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
          />
        </Box>
      )}
      </Box>
    </Box>
  );
};

export default App;
