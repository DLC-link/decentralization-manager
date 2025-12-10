import { useEffect, useState } from "react";
import { Container, Typography, Box, Alert } from "@mui/material";
import { Header } from "./components/Header";
import { PartyCard } from "./components/PartyCard";
import { NodeConfigAccordion } from "./components/NodeConfigAccordion";
import { NetworkConfigAccordion } from "./components/NetworkConfigAccordion";
import { LoadingSkeleton } from "./components/LoadingSkeleton";
import { API_BASE } from "./constants";
import type {
  DecentralizedParty,
  NodeConfig,
  NetworkConfig,
  ParticipantStatus,
} from "./types";

const App = () => {
  const [parties, setParties] = useState<DecentralizedParty[]>([]);
  const [nodeConfig, setNodeConfig] = useState<NodeConfig | null>(null);
  const [networkConfig, setNetworkConfig] = useState<NetworkConfig | null>(null);
  const [participantStatuses, setParticipantStatuses] = useState<ParticipantStatus[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if ("scrollRestoration" in history) {
      history.scrollRestoration = "manual";
    }
    window.scrollTo(0, 0);
  }, []);

  useEffect(() => {
    const fetchData = async () => {
      try {
        const [partiesRes, nodeRes, networkRes] = await Promise.all([
          fetch(`${API_BASE}/decentralized-parties`),
          fetch(`${API_BASE}/node-config`),
          fetch(`${API_BASE}/network-config`),
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
                participantStatuses={participantStatuses}
              />
            )}

            <Box sx={{ mt: 5, mb: 3 }}>
              <Typography variant="h6" sx={{ mb: 0.5 }}>
                Decentralized Parties
              </Typography>
              <Typography variant="body2" color="text.secondary">
                {parties.length} parties
              </Typography>
            </Box>

            {parties.map((party) => (
              <PartyCard key={party.party_id} party={party} />
            ))}
          </>
        )}
      </Container>
    </>
  );
}

export default App;
