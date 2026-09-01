import {
  Box,
  Chip,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  Tooltip,
  Typography,
} from "@mui/material";
import { CopyableText } from "./CopyableText";
import { PaginationControls } from "./Pagination";
import { usePagination } from "../usePagination";
import { zebraRow } from "../styles";
import type { ExternalPartyInfo } from "../types";

interface ExternalPartyListProps {
  parties: ExternalPartyInfo[];
}

/** Render an RFC 3339 timestamp as a readable UTC date + time. */
const formatCreated = (iso: string | null | undefined) => {
  if (!iso) return "—";
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "—";
  return date.toISOString().replace("T", " ").slice(0, 16) + " UTC";
};

export const ExternalPartyList = ({ parties }: ExternalPartyListProps) => {
  const { page, setPage, pageCount, pageItems, total } = usePagination(parties);

  if (parties.length === 0) {
    return (
      <Typography
        variant="body2"
        color="text.secondary"
        sx={{ textAlign: "center", py: 6 }}
      >
        No external parties hosted on this node
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
            <TableCell sx={{ py: 1 }}>
              <Tooltip title="Live means this node holds the party's contracts and confirms for it. Onboarding means the party is assigned here but still carries Canton's onboarding marker: its contracts have not been replicated yet, so it confirms nothing.">
                <span>Status</span>
              </Tooltip>
            </TableCell>
            <TableCell sx={{ py: 1 }} align="right">
              <Tooltip title="How many participants host this party. Any one of them being down does not take the party down.">
                <span>Hosts</span>
              </Tooltip>
            </TableCell>
            <TableCell sx={{ py: 1 }} align="right">
              <Tooltip title="How many of the hosting participants must confirm a transaction involving this party. Separate from the party's signing threshold — one wallet-held key authorizes, this many hosts confirm.">
                <span>Confirmations</span>
              </Tooltip>
            </TableCell>
            <TableCell sx={{ py: 1 }} align="right">
              <Tooltip title="When the hosting mapping became effective in the synchronizer's topology.">
                <span>Created</span>
              </Tooltip>
            </TableCell>
          </TableRow>
        </TableHead>
        <TableBody>
          {pageItems.map((party, idx) => (
            <TableRow key={party.party_id} sx={{ ...zebraRow(idx) }}>
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
              <TableCell sx={{ py: 1.5 }}>
                {party.onboarding ? (
                  <Chip
                    label="Onboarding"
                    size="small"
                    color="warning"
                    variant="outlined"
                  />
                ) : (
                  <Chip
                    label="Live"
                    size="small"
                    color="success"
                    variant="outlined"
                  />
                )}
              </TableCell>
              <TableCell sx={{ py: 1.5 }} align="right">
                <Typography variant="body2">{party.host_count}</Typography>
              </TableCell>
              <TableCell sx={{ py: 1.5 }} align="right">
                <Typography variant="body2">
                  {party.threshold} of {party.host_count}
                </Typography>
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
      <PaginationControls
        page={page}
        pageCount={pageCount}
        total={total}
        onChange={setPage}
      />
    </Box>
  );
};
