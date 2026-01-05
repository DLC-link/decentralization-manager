import { useState } from "react";
import {
  Card,
  CardContent,
  Chip,
  Box,
  Typography,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  IconButton,
  Tooltip,
  Button,
} from "@mui/material";
import PersonRemoveIcon from "@mui/icons-material/PersonRemove";
import UploadFileIcon from "@mui/icons-material/UploadFile";
import { CopyableText } from "./CopyableText";
import { KickDialog } from "./KickDialog";
import { ContractsDialog } from "./ContractsDialog";
import type { DecentralizedParty } from "../types";
import { MAINNET_DEMO } from "../constants";

interface PartyCardProps {
  party: DecentralizedParty;
  onRefresh: () => void;
}

export const PartyCard = ({ party, onRefresh }: PartyCardProps) => {
  const [kickDialogOpen, setKickDialogOpen] = useState(false);
  const [contractsDialogOpen, setContractsDialogOpen] = useState(false);
  const [selectedParticipant, setSelectedParticipant] = useState<string>("");

  const handleKickClick = (participantUid: string) => {
    setSelectedParticipant(participantUid);
    setKickDialogOpen(true);
  };

  const isOwner = Boolean(party.my_owner_key);
  return (
    <Card sx={{ mb: 3, borderRadius: 3 }}>
      <CardContent sx={{ p: 3, "&:last-child": { pb: 3 } }}>
        <CopyableText
          text={party.party_id}
          truncate={{ start: party.party_id.indexOf("::") + 18, end: 16 }}
          variant="h6"
        />

        <Box sx={{ display: "flex", flexWrap: "wrap", gap: 1, mb: 2, mt: 1.5, alignItems: "center" }}>
          <Chip label={`Threshold: ${party.threshold}`} size="small" />
          <Chip label={`Owners: ${party.owners.length}`} size="small" />
          <Chip
            label={`Participants: ${party.participants.length}`}
            size="small"
          />
          {party.contracts && (
            <Chip
              label={`Contracts: ${party.contracts.length}`}
              size="small"
              color="primary"
            />
          )}
          {isOwner && (
            <Button
              variant="outlined"
              size="small"
              startIcon={<UploadFileIcon />}
              onClick={() => setContractsDialogOpen(true)}
              disabled={MAINNET_DEMO}
            >
              Deploy Contracts
            </Button>
          )}
        </Box>

        {party.my_owner_key && (
          <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
            <Typography variant="body2" color="text.secondary">
              <strong>My Owner Key:</strong>
            </Typography>
            <CopyableText
              text={party.my_owner_key}
              truncate={{ start: 16, end: 16 }}
              variant="body2"
            />
          </Box>
        )}

        <Typography variant="subtitle2" sx={{ mt: 3, mb: 1.5 }}>
          Participants
        </Typography>
        <Table size="small">
          <TableHead>
            <TableRow>
              <TableCell>Participant</TableCell>
              <TableCell>Permission</TableCell>
              <TableCell align="right">Actions</TableCell>
            </TableRow>
          </TableHead>
          <TableBody>
            {party.participants.map((p) => (
              <TableRow key={p.participant_uid}>
                <TableCell>
                  <CopyableText
                    text={p.participant_uid}
                    truncate={{ start: 32, end: 16 }}
                    variant="body2"
                  />
                </TableCell>
                <TableCell>
                  <Chip
                    label={p.permission}
                    size="small"
                    color={
                      p.permission === "submission" ? "success" : "default"
                    }
                  />
                </TableCell>
                <TableCell align="right">
                  <Tooltip title="Kick participant">
                    <span>
                      <IconButton
                        size="small"
                        color="error"
                        onClick={() => handleKickClick(p.participant_uid)}
                        disabled={MAINNET_DEMO}
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

        {party.contracts && party.contracts.length > 0 && (
          <>
            <Typography variant="subtitle2" sx={{ mt: 3, mb: 1.5 }}>
              Contracts
            </Typography>
            <Table size="small">
              <TableHead>
                <TableRow>
                  <TableCell>Template</TableCell>
                  <TableCell>Contract ID</TableCell>
                </TableRow>
              </TableHead>
              <TableBody>
                {party.contracts.map((c) => (
                  <TableRow key={c.contract_id}>
                    <TableCell>{c.template_id}</TableCell>
                    <TableCell>
                      <CopyableText
                        text={c.contract_id}
                        truncate={{ start: 16, end: 16 }}
                        variant="caption"
                      />
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </>
        )}
      </CardContent>

      <KickDialog
        open={kickDialogOpen}
        onClose={() => setKickDialogOpen(false)}
        onKickComplete={onRefresh}
        partyId={party.party_id}
        participantUid={selectedParticipant}
        currentThreshold={party.threshold}
        currentOwnerCount={party.owners.length}
      />

      <ContractsDialog
        open={contractsDialogOpen}
        onClose={() => setContractsDialogOpen(false)}
        onComplete={onRefresh}
        partyId={party.party_id}
      />
    </Card>
  );
};
