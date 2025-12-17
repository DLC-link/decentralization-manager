import { useEffect, useState, useCallback } from "react";
import { Container, Typography, Box, Alert, IconButton, Tooltip, Button } from "@mui/material";
import VpnKeyIcon from "@mui/icons-material/VpnKey";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import AddIcon from "@mui/icons-material/Add";
import { Header } from "./components/Header";
import { PartyCard } from "./components/PartyCard";
import { NodeConfigAccordion } from "./components/NodeConfigAccordion";
import { NetworkConfigAccordion } from "./components/NetworkConfigAccordion";
import { LoadingSkeleton } from "./components/LoadingSkeleton";
import { OnboardingDialog } from "./components/OnboardingDialog";
import { copyToClipboard } from "./components/CopyableText";
import { useSnackbar } from "./contexts";
import { API_BASE } from "./constants";
import type {
  DecentralizedParty,
  NodeConfig,
  NetworkConfig,
  ParticipantStatus,
  KeyStatusResponse,
  Peer,
} from "./types";

const App = () => {
  const [parties, setParties] = useState<DecentralizedParty[]>([]);
  const [nodeConfig, setNodeConfig] = useState<NodeConfig | null>(null);
  const [networkConfig, setNetworkConfig] = useState<NetworkConfig | null>(null);
  const [participantStatuses, setParticipantStatuses] = useState<ParticipantStatus[]>([]);
  const [keyStatus, setKeyStatus] = useState<KeyStatusResponse | null>(null);
  const [onboardingDialogOpen, setOnboardingDialogOpen] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const { showSnackbar } = useSnackbar();

  useEffect(() => {
    if ("scrollRestoration" in history) {
      history.scrollRestoration = "manual";
    }
    window.scrollTo(0, 0);
  }, []);

  const refreshParties = useCallback(async () => {
    try {
      const res = await fetch(`${API_BASE}/decentralized-parties`);
      if (res.ok) {
        const data = await res.json();
        setParties(data.parties);
      } else {
        showSnackbar("Failed to refresh parties");
      }
    } catch (err) {
      showSnackbar(err instanceof Error ? err.message : "Failed to refresh parties");
    }
  }, [showSnackbar]);

  const savePeers = useCallback(async (peers: Peer[]) => {
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
  }, [showSnackbar]);

  useEffect(() => {
    const fetchData = async () => {
      try {
        const [partiesRes, nodeRes, networkRes, keyStatusRes] = await Promise.all([
          fetch(`${API_BASE}/decentralized-parties`),
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
            {keyStatus && keyStatus.has_keys && keyStatus.public_key && (
              <Alert
                severity="success"
                sx={{ mb: 2, borderRadius: 3 }}
                icon={<VpnKeyIcon />}
                action={
                  <Tooltip title="Copy public key">
                    <IconButton
                      size="small"
                      color="inherit"
                      onClick={async () => {
                        const success = await copyToClipboard(keyStatus.public_key!);
                        showSnackbar(success ? "Copied to clipboard" : "Failed to copy");
                      }}
                    >
                      <ContentCopyIcon fontSize="small" />
                    </IconButton>
                  </Tooltip>
                }
              >
                <Typography variant="body2" component="span">
                  <strong>Public Key:</strong>{" "}
                  <code style={{ fontSize: "0.85em", wordBreak: "break-all" }}>
                    {keyStatus.public_key}
                  </code>
                </Typography>
              </Alert>
            )}
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

            <Box sx={{ mt: 5, mb: 3, display: "flex", justifyContent: "space-between", alignItems: "flex-start" }}>
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
              >
                Create Party
              </Button>
            </Box>

            {parties.map((party) => (
              <PartyCard key={party.party_id} party={party} onRefresh={refreshParties} />
            ))}

            <OnboardingDialog
              open={onboardingDialogOpen}
              onClose={() => setOnboardingDialogOpen(false)}
              onComplete={refreshParties}
            />
          </>
        )}
      </Container>
    </>
  );
}

export default App;
