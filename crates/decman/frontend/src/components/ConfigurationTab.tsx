import { Box, Divider } from "@mui/material";
import { ConfigTabSkeleton } from "./LoadingSkeleton";
import { NetworkConfigAccordion } from "./NetworkConfigAccordion";
import { NodeHealthCard } from "./NodeHealthCard";
import type {
  KeyStatusResponse,
  NetworkConfig,
  NodeConfig,
  NodeHealthResponse,
  ParticipantStatus,
  Peer,
} from "../types";

interface ConfigurationTabProps {
  isLargeScreen: boolean;
  nodeConfig: NodeConfig | null;
  nodeHealth: NodeHealthResponse | null;
  selfLatency: { ms?: number; seq: number };
  networkConfig: NetworkConfig | null;
  keyStatus: KeyStatusResponse | null;
  participantStatuses: ParticipantStatus[];
  selfWireVersion: number | undefined;
  savePeers: (peers: Peer[]) => Promise<void>;
  configPeerId: string | null;
  showSharedParties: (participantId: string) => void;
}

/** The configuration tab: node health and the network configuration. */
export const ConfigurationTab = ({
  isLargeScreen,
  nodeConfig,
  nodeHealth,
  selfLatency,
  networkConfig,
  keyStatus,
  participantStatuses,
  selfWireVersion,
  savePeers,
  configPeerId,
  showSharedParties,
}: ConfigurationTabProps) => (
  <Box sx={{ pt: isLargeScreen ? 4 : 0 }}>
    {nodeConfig ? (
      <>
        <Box sx={{ px: "var(--content-pad)", py: 2 }}>
          <NodeHealthCard
            config={nodeConfig}
            health={nodeHealth}
            selfLatency={selfLatency}
          />
        </Box>
        <Divider />
        {networkConfig ? (
          <NetworkConfigAccordion
            config={networkConfig}
            nodeConfig={nodeConfig ?? undefined}
            keyStatus={keyStatus ?? undefined}
            participantStatuses={participantStatuses}
            selfWireVersion={selfWireVersion}
            selfLatencyMs={selfLatency.ms}
            onSave={savePeers}
            focusParticipantId={configPeerId}
            onShowSharedParties={showSharedParties}
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
);
