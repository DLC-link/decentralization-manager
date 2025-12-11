import { useEffect, useState } from "react";
import { Container, Typography, Box, Alert, Button, CircularProgress, IconButton, Tooltip } from "@mui/material";
import VpnKeyIcon from "@mui/icons-material/VpnKey";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import AddIcon from "@mui/icons-material/Add";
import { Header } from "./components/Header";
import { PartyCard } from "./components/PartyCard";
import { NodeConfigAccordion } from "./components/NodeConfigAccordion";
import { NetworkConfigAccordion } from "./components/NetworkConfigAccordion";
import { LoadingSkeleton } from "./components/LoadingSkeleton";
import { OnboardingDialog } from "./components/OnboardingDialog";
import { useSnackbar } from "./contexts";
import { API_BASE } from "./constants";
import type {
  DecentralizedParty,
  NodeConfig,
  NetworkConfig,
  ParticipantStatus,
  KeyStatusResponse,
} from "./types";

const App = () => {
  const [parties, setParties] = useState<DecentralizedParty[]>([]);
  const [nodeConfig, setNodeConfig] = useState<NodeConfig | null>(null);
  const [networkConfig, setNetworkConfig] = useState<NetworkConfig | null>(null);
  const [participantStatuses, setParticipantStatuses] = useState<ParticipantStatus[]>([]);
  const [keyStatus, setKeyStatus] = useState<KeyStatusResponse | null>(null);
  const [generatingKeys, setGeneratingKeys] = useState(false);
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

        // Fetch participant statuses (non-blocking)
        fetch(`${API_BASE}/participants-status`)
          .then((res) => res.ok ? res.json() : null)
          .then((data) => data && setParticipantStatuses(data.statuses))
          .catch(() => {});
      } catch (err) {
        setError(err instanceof Error ? err.message : "Unknown error");
      } finally {
        setLoading(false);
      }
    };

    fetchData();
  }, []);

  const handleGenerateKeys = async () => {
    setGeneratingKeys(true);
    try {
      const res = await fetch(`${API_BASE}/keys/generate`, { method: "POST" });
      const data = await res.json();

      if (res.ok && data.success) {
        setKeyStatus({ has_keys: true, public_key: data.public_key });
        await navigator.clipboard.writeText(data.public_key);
        showSnackbar("Keys generated and public key copied to clipboard");
      } else {
        showSnackbar(data.error || "Failed to generate keys");
      }
    } catch {
      showSnackbar("Failed to generate keys");
    } finally {
      setGeneratingKeys(false);
    }
  };

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
            {keyStatus && !keyStatus.has_keys && (
              <Alert
                severity="warning"
                sx={{ mb: 2, borderRadius: 3 }}
                action={
                  <Button
                    color="inherit"
                    size="small"
                    onClick={handleGenerateKeys}
                    disabled={generatingKeys}
                    startIcon={generatingKeys ? <CircularProgress size={16} /> : <VpnKeyIcon />}
                  >
                    {generatingKeys ? "Generating..." : "Generate Keys"}
                  </Button>
                }
              >
                No Noise protocol keys found. Generate keys to enable secure communication with other nodes.
              </Alert>
            )}
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
                      onClick={() => {
                        navigator.clipboard.writeText(keyStatus.public_key!);
                        showSnackbar("Public key copied to clipboard");
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
                participantStatuses={participantStatuses}
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
              <PartyCard key={party.party_id} party={party} />
            ))}

            <OnboardingDialog
              open={onboardingDialogOpen}
              onClose={() => setOnboardingDialogOpen(false)}
            />
          </>
        )}
      </Container>
    </>
  );
}

export default App;
