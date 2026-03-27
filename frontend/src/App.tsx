import { useEffect, useState, useCallback } from "react";
import {
  Container,
  Typography,
  Box,
  Alert,
  Button,
  TextField,
  InputAdornment,
  LinearProgress,
  IconButton,
} from "@mui/material";
import FilterListIcon from "@mui/icons-material/FilterList";
import SearchIcon from "@mui/icons-material/Search";
import AddIcon from "@mui/icons-material/Add";
import CloudUploadIcon from "@mui/icons-material/CloudUpload";
import { Header } from "./components/Header";
import { PartyCard } from "./components/PartyCard";
import { NodeConfigAccordion } from "./components/NodeConfigAccordion";
import { NetworkConfigAccordion } from "./components/NetworkConfigAccordion";
import { VettedPackagesAccordion } from "./components/VettedPackagesAccordion";
import { LoadingSkeleton } from "./components/LoadingSkeleton";
import { DarsDialog } from "./components/DarsDialog";
import { OnboardingDialog } from "./components/OnboardingDialog";
import { InvitationModal } from "./components/InvitationModal";
import { useSnackbar } from "./contexts";
import { API_BASE, ADMIN_ACCESS, OPERATOR_API_URLS } from "./constants";
import type {
  DecentralizedParty,
  DecentralizedPartiesResponse,
  VettedPackageInfo,
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
  const [parties, setParties] = useState<DecentralizedParty[]>([]);
  const [vettedPackages, setVettedPackages] = useState<VettedPackageInfo[]>([]);
  const [nodeConfig, setNodeConfig] = useState<NodeConfig | null>(null);
  const [networkConfig, setNetworkConfig] = useState<NetworkConfig | null>(
    null,
  );
  const [participantStatuses, setParticipantStatuses] = useState<
    ParticipantStatus[]
  >([]);
  const [keyStatus, setKeyStatus] = useState<KeyStatusResponse | null>(null);
  const [authStatuses, setAuthStatuses] = useState<PartyAuthStatus[]>([]);
  const [onboardingDialogOpen, setOnboardingDialogOpen] = useState(false);
  const [darsDialogOpen, setDarsDialogOpen] = useState(false);
  const [_pendingInvitations, setPendingInvitations] = useState<
    PendingInvitation[]
  >([]);
  const [currentInvitation, setCurrentInvitation] =
    useState<PendingInvitation | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [partyFilter, setPartyFilter] = useState("cbtc-network");
  const [refreshingParties, setRefreshingParties] = useState(false);
  const [operatorParty, setOperatorParty] = useState("");
  const { showSnackbar } = useSnackbar();

  useEffect(() => {
    if ("scrollRestoration" in history) {
      history.scrollRestoration = "manual";
    }
    window.scrollTo(0, 0);
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
        setVettedPackages(data.vetted_packages ?? []);
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
        const [partiesRes, networkRes, keyStatusRes, authStatusRes] =
          await Promise.all([
            fetch(`${API_BASE}/decentralized-parties${partiesParams}`),
            fetch(`${API_BASE}/network-config`),
            fetch(`${API_BASE}/keys/status`),
            fetch(`${API_BASE}/auth/status`),
          ]);

        if (!partiesRes.ok || !networkRes.ok) {
          throw new Error("Failed to fetch data");
        }

        const partiesData: DecentralizedPartiesResponse = await partiesRes.json();
        const networkData = await networkRes.json();

        setParties(partiesData.parties);
        setVettedPackages(partiesData.vetted_packages ?? []);
        setNetworkConfig(networkData);

        if (keyStatusRes.ok) {
          const keyStatusData = await keyStatusRes.json();
          setKeyStatus(keyStatusData);
        }

        if (authStatusRes.ok) {
          const authStatusData: AuthStatusResponse = await authStatusRes.json();
          setAuthStatuses(authStatusData.parties);
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
      <Header />

      <Container maxWidth="md" sx={{ pt: 16, pb: 6 }}>
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
            {nodeConfig && <NodeConfigAccordion config={nodeConfig} />}
            {networkConfig && (
              <NetworkConfigAccordion
                config={networkConfig}
                nodeConfig={nodeConfig ?? undefined}
                keyStatus={keyStatus ?? undefined}
                participantStatuses={participantStatuses}
                onSave={savePeers}
              />
            )}
            {vettedPackages.length > 0 && (
              <VettedPackagesAccordion packages={vettedPackages} />
            )}

            <Box sx={{ mt: 5, mb: 3 }}>
              <Box
                sx={{
                  display: "flex",
                  justifyContent: "space-between",
                  alignItems: "flex-start",
                  mb: 2,
                }}
              >
                <Box>
                  <Typography variant="h6" sx={{ mb: 0.5 }}>
                    Decentralized Parties
                  </Typography>
                  <Typography variant="body2" color="text.secondary">
                    {parties.length} parties
                  </Typography>
                </Box>
                <Box sx={{ display: "flex", gap: 1 }}>
                  <Button
                    variant="contained"
                    color="secondary"
                    startIcon={<CloudUploadIcon />}
                    onClick={() => setDarsDialogOpen(true)}
                    disabled={!ADMIN_ACCESS}
                  >
                    Upload DARs
                  </Button>
                  <Button
                    variant="contained"
                    startIcon={<AddIcon />}
                    onClick={() => setOnboardingDialogOpen(true)}
                    disabled={!ADMIN_ACCESS}
                  >
                    Create Party
                  </Button>
                </Box>
              </Box>
              <Box sx={{ display: "flex", alignItems: "flex-start", gap: 1 }}>
                <TextField
                  size="small"
                  label="Filter by prefix"
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

            {parties.map((party) => (
              <PartyCard
                key={party.party_id}
                party={party}
                onRefresh={refreshParties}
                selfParticipantId={nodeConfig?.node.participant_id}
                authStatus={authStatuses.find((a) => a.dec_party_id === party.party_id)}
                onAuthRefresh={refreshAuthStatus}
                operatorParty={operatorParty}
                network={nodeConfig?.canton.network}
                vettedPackages={vettedPackages}
              />
            ))}

            <DarsDialog
              open={darsDialogOpen}
              onClose={() => setDarsDialogOpen(false)}
              onComplete={refreshParties}
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
    </Box>
  );
};

export default App;
