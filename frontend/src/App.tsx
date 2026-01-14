import { useEffect, useState, useCallback } from "react";
import {
  Container,
  Typography,
  Box,
  Alert,
  Button,
  TextField,
  InputAdornment,
} from "@mui/material";
import FilterListIcon from "@mui/icons-material/FilterList";
import AddIcon from "@mui/icons-material/Add";
import { Header } from "./components/Header";
import { PartyCard } from "./components/PartyCard";
import { NodeConfigAccordion } from "./components/NodeConfigAccordion";
import { NetworkConfigAccordion } from "./components/NetworkConfigAccordion";
import { LoadingSkeleton } from "./components/LoadingSkeleton";
import { OnboardingDialog } from "./components/OnboardingDialog";
import { InvitationModal } from "./components/InvitationModal";
import { useSnackbar } from "./contexts";
import { API_BASE, MAINNET_DEMO } from "./constants";
import type {
  DecentralizedParty,
  NodeConfig,
  NetworkConfig,
  ParticipantStatus,
  KeyStatusResponse,
  Peer,
  PendingInvitation,
} from "./types";

const App = () => {
  const [parties, setParties] = useState<DecentralizedParty[]>([]);
  const [nodeConfig, setNodeConfig] = useState<NodeConfig | null>(null);
  const [networkConfig, setNetworkConfig] = useState<NetworkConfig | null>(
    null,
  );
  const [participantStatuses, setParticipantStatuses] = useState<
    ParticipantStatus[]
  >([]);
  const [keyStatus, setKeyStatus] = useState<KeyStatusResponse | null>(null);
  const [onboardingDialogOpen, setOnboardingDialogOpen] = useState(false);
  const [_pendingInvitations, setPendingInvitations] = useState<
    PendingInvitation[]
  >([]);
  const [currentInvitation, setCurrentInvitation] =
    useState<PendingInvitation | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [partyFilter, setPartyFilter] = useState("cbtc-network");
  const { showSnackbar } = useSnackbar();

  useEffect(() => {
    if ("scrollRestoration" in history) {
      history.scrollRestoration = "manual";
    }
    window.scrollTo(0, 0);
  }, []);

  const refreshParties = useCallback(async () => {
    try {
      const params = partyFilter
        ? `?prefix=${encodeURIComponent(partyFilter)}`
        : "";
      const res = await fetch(`${API_BASE}/decentralized-parties${params}`);
      if (res.ok) {
        const data = await res.json();
        setParties(data.parties);
      } else {
        showSnackbar("Failed to refresh parties");
      }
    } catch (err) {
      showSnackbar(
        err instanceof Error ? err.message : "Failed to refresh parties",
      );
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

  useEffect(() => {
    const fetchData = async () => {
      try {
        const partiesParams = partyFilter
          ? `?prefix=${encodeURIComponent(partyFilter)}`
          : "";
        const [partiesRes, nodeRes, networkRes, keyStatusRes] =
          await Promise.all([
            fetch(`${API_BASE}/decentralized-parties${partiesParams}`),
            fetch(`${API_BASE}/node-config`),
            fetch(`${API_BASE}/network-config`),
            fetch(`${API_BASE}/keys/status`),
          ]);

        if (!partiesRes.ok || !nodeRes.ok || !networkRes.ok) {
          throw new Error("Failed to fetch data");
        }

        const partiesData = await partiesRes.json();
        const nodeData = await nodeRes.json();
        const networkData = await networkRes.json();

        setParties(partiesData.parties);
        setNodeConfig(nodeData);
        setNetworkConfig(networkData);

        if (keyStatusRes.ok) {
          const keyStatusData = await keyStatusRes.json();
          setKeyStatus(keyStatusData);
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
    <>
      <Header />

      <Container maxWidth="md" sx={{ pt: 16, pb: 6 }}>
        {loading ? (
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
                <Button
                  variant="contained"
                  startIcon={<AddIcon />}
                  onClick={() => setOnboardingDialogOpen(true)}
                  disabled={MAINNET_DEMO}
                >
                  Create Party
                </Button>
              </Box>
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
                InputProps={{
                  startAdornment: (
                    <InputAdornment position="start">
                      <FilterListIcon fontSize="small" color="action" />
                    </InputAdornment>
                  ),
                }}
                sx={{ width: 300 }}
                helperText="Press Enter to apply filter"
              />
            </Box>

            {parties.map((party) => (
              <PartyCard
                key={party.party_id}
                party={party}
                onRefresh={refreshParties}
                selfParticipantId={nodeConfig?.node.participant_id}
              />
            ))}

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
    </>
  );
};

export default App;
