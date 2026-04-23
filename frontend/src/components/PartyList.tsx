import {
  Box,
  Chip,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  Typography,
} from "@mui/material";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import ErrorIcon from "@mui/icons-material/Error";
import ScienceIcon from "@mui/icons-material/Science";
import { CopyableText } from "./CopyableText";
import { zebraRow } from "../styles";
import type { DecentralizedParty, PartyAuthStatus } from "../types";

interface PartyListProps {
  parties: DecentralizedParty[];
  authStatuses: PartyAuthStatus[];
  onSelectParty: (partyId: string) => void;
}

const AuthStatusIcon = ({ status }: { status?: PartyAuthStatus }) => {
  if (!status) return null;
  switch (status.status.status) {
    case "authenticated":
      return <CheckCircleIcon color="success" sx={{ fontSize: 18 }} />;
    case "mock":
      return <ScienceIcon color="warning" sx={{ fontSize: 18 }} />;
    case "failed":
      return <ErrorIcon color="error" sx={{ fontSize: 18 }} />;
    default:
      return null;
  }
};

export const PartyList = ({
  parties,
  authStatuses,
  onSelectParty,
}: PartyListProps) => {
  if (parties.length === 0) {
    return (
      <Typography variant="body2" color="text.secondary" sx={{ textAlign: "center", py: 6 }}>
        No parties found
      </Typography>
    );
  }

  return (
    <Box>
      <Table size="small">
        <TableHead>
          <TableRow>
            <TableCell sx={{ py: 1 }}>Party ID</TableCell>
            <TableCell sx={{ py: 1 }} align="center">Threshold</TableCell>
            <TableCell sx={{ py: 1 }} align="center">Owners</TableCell>
            <TableCell sx={{ py: 1 }} align="center">Participants</TableCell>
            <TableCell sx={{ py: 1 }} align="center">Contracts</TableCell>
            <TableCell sx={{ py: 1 }} align="center">Auth</TableCell>
          </TableRow>
        </TableHead>
        <TableBody>
          {parties.map((party, idx) => {
            const auth = authStatuses.find(
              (a) => a.dec_party_id === party.party_id,
            );
            return (
              <TableRow
                key={party.party_id}
                tabIndex={0}
                sx={{
                  ...zebraRow(idx),
                  cursor: "pointer",
                }}
                onClick={() => onSelectParty(party.party_id)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") onSelectParty(party.party_id);
                }}
              >
                <TableCell sx={{ py: 1.5 }}>
                  <CopyableText
                    text={party.party_id}
                    truncate={{
                      start: party.party_id.indexOf("::") + 18,
                      end: 16,
                    }}
                    variant="body2"
                  />
                </TableCell>
                <TableCell sx={{ py: 1.5 }} align="center">
                  {party.threshold}
                </TableCell>
                <TableCell sx={{ py: 1.5 }} align="center">
                  {party.owners.length}
                </TableCell>
                <TableCell sx={{ py: 1.5 }} align="center">
                  {party.participants.length}
                </TableCell>
                <TableCell sx={{ py: 1.5 }} align="center">
                  {party.contracts ? (
                    <Chip
                      label={party.contracts.length}
                      size="small"
                      color={party.contracts.length > 0 ? "primary" : "default"}
                    />
                  ) : (
                    "-"
                  )}
                </TableCell>
                <TableCell sx={{ py: 1.5 }} align="center">
                  <AuthStatusIcon status={auth} />
                </TableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
    </Box>
  );
};
