import {
  Box,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  Typography,
} from "@mui/material";
import { CopyableText } from "./CopyableText";
import { zebraRow } from "../styles";
import type { ExternalPartyInfo } from "../types";

interface ExternalPartyListProps {
  parties: ExternalPartyInfo[];
}

const formatCreated = (unixSecs: number): string =>
  unixSecs ? new Date(unixSecs * 1000).toLocaleString() : "-";

export const ExternalPartyList = ({ parties }: ExternalPartyListProps) => {
  if (parties.length === 0) {
    return (
      <Typography
        variant="body2"
        color="text.secondary"
        sx={{ textAlign: "center", py: 6 }}
      >
        No external parties found
      </Typography>
    );
  }

  return (
    <Box>
      <Table size="small">
        <TableHead>
          <TableRow>
            <TableCell sx={{ py: 1 }}>Party ID</TableCell>
            <TableCell sx={{ py: 1 }}>Fingerprint</TableCell>
            <TableCell sx={{ py: 1 }} align="right">
              Created
            </TableCell>
          </TableRow>
        </TableHead>
        <TableBody>
          {parties.map((party, idx) => (
            <TableRow key={party.instance_name} sx={{ ...zebraRow(idx) }}>
              <TableCell sx={{ py: 1.5 }}>
                <CopyableText
                  text={party.party_id}
                  truncate={{ start: party.party_id.indexOf("::") + 10, end: 12 }}
                  variant="body2"
                />
              </TableCell>
              <TableCell sx={{ py: 1.5 }}>
                <CopyableText
                  text={party.fingerprint}
                  truncate={{ start: 10, end: 8 }}
                  variant="body2"
                />
              </TableCell>
              <TableCell sx={{ py: 1.5 }} align="right">
                <Typography variant="caption" color="text.secondary">
                  {formatCreated(party.created_at)}
                </Typography>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </Box>
  );
};
