import { Box, Chip, IconButton, Table, TableBody, TableCell, TableHead, TableRow, Tooltip, Typography } from "@mui/material";
import PersonRemoveIcon from "@mui/icons-material/PersonRemove";
import { CopyableText } from "./CopyableText";
import { HostingParticipant } from "./HostingParticipant";
import { cardTableSx, zebraRow } from "../styles";
import { ADMIN_ACCESS } from "../constants";
import type { DecentralizedParty, NonHostingOwner, Peer } from "../types";

interface PartyParticipantsTableProps {
  party: DecentralizedParty;
  nonHostingOwners: NonHostingOwner[];
  selfParticipantId?: string;
  peers: Peer[] | null;
  sharedCounts: Map<string, number> | null;
  onOpenPeerConfig?: (participantId: string) => void;
  onShowSharedParties?: (participantId: string) => void;
  handleKickClick: (participantUid: string) => void;
  handleKickOwnerClick: (owner: NonHostingOwner) => void;
}

/** The Participants section body: hosting participants, then owners that do not host. */
const PartyParticipantsTable = ({
  party,
  nonHostingOwners,
  selfParticipantId,
  peers,
  sharedCounts,
  onOpenPeerConfig,
  onShowSharedParties,
  handleKickClick,
  handleKickOwnerClick,
}: PartyParticipantsTableProps) => (
  <Box sx={{ overflowX: "auto", ...cardTableSx }}>
    <Table size="small">
      <TableHead>
        <TableRow>
          <TableCell sx={{ py: 1 }}>Participant</TableCell>
          <TableCell sx={{ py: 1 }}>Permission</TableCell>
          <TableCell sx={{ py: 1 }} align="right">
            Actions
          </TableCell>
        </TableRow>
      </TableHead>
      <TableBody>
        {party.participants.map((p, idx) => (
          <TableRow
            key={p.participant_uid}
            sx={zebraRow(idx)}
            data-testid="participant-row"
            data-participant-uid={p.participant_uid}
          >
            <TableCell sx={{ py: 1 }}>
              <HostingParticipant
                participantId={p.participant_uid}
                isSelf={p.participant_uid === selfParticipantId}
                peers={peers}
                sharedCount={sharedCounts?.get(p.participant_uid) ?? null}
                onOpenPeerConfig={onOpenPeerConfig}
                onShowSharedParties={onShowSharedParties}
              />
            </TableCell>
            <TableCell sx={{ py: 1 }}>
              <Chip
                label={p.permission}
                size="small"
                color={
                  p.permission === "submission" ? "success" : "default"
                }
              />
            </TableCell>
            <TableCell sx={{ py: 1 }} align="right">
              <Tooltip
                title={
                  p.participant_uid === selfParticipantId
                    ? "Cannot kick yourself"
                    : "Kick participant"
                }
              >
                <span>
                  <IconButton
                    size="small"
                    color="error"
                    aria-label={
                      p.participant_uid === selfParticipantId
                        ? "Cannot kick yourself"
                        : "Kick participant"
                    }
                    onClick={() => handleKickClick(p.participant_uid)}
                    disabled={
                      !ADMIN_ACCESS ||
                      p.participant_uid === selfParticipantId
                    }
                  >
                    <PersonRemoveIcon fontSize="small" />
                  </IconButton>
                </span>
              </Tooltip>
            </TableCell>
          </TableRow>
        ))}
        {nonHostingOwners.map((owner, idx) => (
          <TableRow
            key={owner.owner_key}
            sx={zebraRow(party.participants.length + idx)}
            data-testid="owner-only-row"
            data-owner-key={owner.owner_key}
          >
            <TableCell sx={{ py: 1 }}>
              {owner.participant_uid ? (
                <HostingParticipant
                  participantId={owner.participant_uid}
                  isSelf={false}
                  peers={peers}
                  sharedCount={
                    sharedCounts?.get(owner.participant_uid) ?? null
                  }
                  onOpenPeerConfig={onOpenPeerConfig}
                  onShowSharedParties={onShowSharedParties}
                />
              ) : (
                <Box>
                  <Typography variant="body2" sx={{ fontWeight: 500 }}>
                    Unclaimed owner key
                  </Typography>
                  <CopyableText
                    text={owner.owner_key}
                    truncate={{ start: 10, end: 8 }}
                    variant="caption"
                  />
                </Box>
              )}
            </TableCell>
            <TableCell sx={{ py: 1 }}>
              <Tooltip title="Owns the party's namespace key but does not host the party">
                <Chip label="Owner only" size="small" variant="outlined" />
              </Tooltip>
            </TableCell>
            <TableCell sx={{ py: 1 }} align="right">
              <Tooltip title="Kick owner">
                <span>
                  <IconButton
                    size="small"
                    color="error"
                    aria-label="Kick owner"
                    onClick={() => handleKickOwnerClick(owner)}
                    disabled={!ADMIN_ACCESS}
                  >
                    <PersonRemoveIcon fontSize="small" />
                  </IconButton>
                </span>
              </Tooltip>
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  </Box>
);

export { PartyParticipantsTable };
