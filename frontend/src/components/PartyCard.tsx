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
} from "@mui/material";
import { CopyableText } from "./CopyableText";
import type { DecentralizedParty } from "../types";

interface PartyCardProps {
  party: DecentralizedParty;
}

export const PartyCard = ({ party }: PartyCardProps) => {
  return (
    <Card sx={{ mb: 3, borderRadius: 3 }}>
      <CardContent sx={{ p: 3, "&:last-child": { pb: 3 } }}>
        <CopyableText
          text={party.party_id}
          truncate={{ start: party.party_id.indexOf("::") + 18, end: 16 }}
          variant="h6"
        />

        <Box sx={{ display: "flex", flexWrap: "wrap", gap: 1, mb: 2, mt: 1.5 }}>
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
            </TableRow>
          </TableHead>
          <TableBody>
            {party.participants.map((p) => (
              <TableRow key={p.participant_uid}>
                <TableCell>
                  <CopyableText
                    text={p.participant_uid}
                    truncate={{ start: 32, end: 32 }}
                    variant="body2"
                  />
                </TableCell>
                <TableCell>
                  <Chip
                    label={p.permission}
                    size="small"
                    color={p.permission === "submission" ? "success" : "default"}
                  />
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
    </Card>
  );
}
