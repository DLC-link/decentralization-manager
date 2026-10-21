import type { ReactNode } from "react";
import { Box, Link, Tooltip, Typography } from "@mui/material";
import { CopyableText } from "./CopyableText";
import { SharedPartiesChip } from "./SharedPartiesChip";
import { participantName } from "../sharedParties";
import type { Peer } from "../types";

interface HostingParticipantProps {
  participantId: string;
  isSelf: boolean;
  /// Null while the peers are loading.
  peers: Peer[] | null;
  /// Parties this node shares with the participant. Null while loading.
  sharedCount: number | null;
  onOpenPeerConfig?: (participantId: string) => void;
  onShowSharedParties?: (participantId: string) => void;
}

/**
 * One hosting participant in a party's Participants table: its peer name over
 * its id. The name links to the peer's configuration row, and a chip opens the
 * parties this node shares with it.
 */
export const HostingParticipant = ({
  participantId,
  isSelf,
  peers,
  sharedCount,
  onOpenPeerConfig,
  onShowSharedParties,
}: HostingParticipantProps) => {
  const peer = peers?.find((p) => p.participant_id === participantId);
  const name = participantName(participantId, peers);

  let label: ReactNode = null;
  if (isSelf) {
    label = (
      <Typography variant="body2" sx={{ fontWeight: 500 }}>
        {name} (You)
      </Typography>
    );
  } else if (peer && onOpenPeerConfig) {
    label = (
      <Tooltip title="Open this peer's configuration" describeChild>
        <Link
          component="button"
          variant="body2"
          underline="hover"
          onClick={() => onOpenPeerConfig(participantId)}
          sx={{ fontWeight: 500 }}
        >
          {name}
        </Link>
      </Tooltip>
    );
  } else if (peer) {
    label = (
      <Typography variant="body2" sx={{ fontWeight: 500 }}>
        {name}
      </Typography>
    );
  } else if (peers) {
    label = (
      <Typography variant="body2" color="text.secondary">
        Not a configured peer
      </Typography>
    );
  }

  // This node hosts every listed party, so a count for itself says nothing.
  const chip =
    !isSelf && sharedCount !== null && onShowSharedParties ? (
      <SharedPartiesChip
        count={sharedCount}
        name={name}
        onClick={() => onShowSharedParties(participantId)}
      />
    ) : null;

  return (
    <Box>
      {(label || chip) && (
        <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
          {label}
          {chip}
        </Box>
      )}
      <CopyableText
        text={participantId}
        truncate={{ start: 32, end: 16 }}
        variant="caption"
      />
    </Box>
  );
};
