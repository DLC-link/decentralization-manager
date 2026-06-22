import {
  Box,
  IconButton,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  Tooltip,
  Typography,
} from "@mui/material";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import ErrorIcon from "@mui/icons-material/Error";
import ScienceIcon from "@mui/icons-material/Science";
import VisibilityIcon from "@mui/icons-material/Visibility";
import VisibilityOffIcon from "@mui/icons-material/VisibilityOff";
import { CopyableText } from "./CopyableText";
import { zebraRow } from "../styles";
import type { DecentralizedParty, PartyAuthStatus } from "../types";

interface PartyListProps {
  parties: DecentralizedParty[];
  authStatuses: PartyAuthStatus[];
  onSelectParty: (partyId: string) => void;
  isHidden: (partyId: string) => boolean;
  onToggleHidden: (partyId: string) => void;
}

const AuthStatusIcon = ({ status }: { status?: PartyAuthStatus }) => {
  if (!status) return null;
  switch (status.status.status) {
    case "authenticated":
      return (
        <Tooltip title="Authenticated">
          <CheckCircleIcon color="success" sx={{ fontSize: 18 }} />
        </Tooltip>
      );
    case "mock":
      return (
        <Tooltip title="Test mode (mock authentication)">
          <ScienceIcon color="warning" sx={{ fontSize: 18 }} />
        </Tooltip>
      );
    case "failed":
      return (
        <Tooltip title="Authentication failed">
          <ErrorIcon color="error" sx={{ fontSize: 18 }} />
        </Tooltip>
      );
    default:
      return null;
  }
};

export const PartyList = ({
  parties,
  authStatuses,
  onSelectParty,
  isHidden,
  onToggleHidden,
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
            <TableCell sx={{ py: 1 }} aria-hidden />
            <TableCell sx={{ py: 1, width: "1%", whiteSpace: "nowrap" }}>
              Party ID
            </TableCell>
            <TableCell
              sx={{ py: 1, width: "1%", whiteSpace: "nowrap", pl: 4 }}
              align="center"
            >
              Auth
            </TableCell>
            <TableCell sx={{ py: 1, width: 56 }} align="center" />
            <TableCell sx={{ py: 1 }} aria-hidden />
          </TableRow>
        </TableHead>
        <TableBody>
          {parties.map((party, idx) => {
            const auth = authStatuses.find(
              (a) => a.dec_party_id === party.party_id,
            );
            const hidden = isHidden(party.party_id);
            return (
              <TableRow
                key={party.party_id}
                tabIndex={0}
                sx={{
                  ...zebraRow(idx),
                  cursor: "pointer",
                  opacity: hidden ? 0.45 : 1,
                }}
                onClick={() => onSelectParty(party.party_id)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") onSelectParty(party.party_id);
                }}
              >
                <TableCell sx={{ py: 1 }} aria-hidden />
                <TableCell sx={{ py: 1, width: "1%", whiteSpace: "nowrap" }}>
                  <CopyableText
                    text={party.party_id}
                    truncate={{
                      start: party.party_id.indexOf("::") + 18,
                      end: 16,
                    }}
                    expandOnWide
                    variant="body2"
                  />
                </TableCell>
                <TableCell
                  sx={{ py: 1, width: "1%", whiteSpace: "nowrap", pl: 4 }}
                  align="center"
                >
                  <AuthStatusIcon status={auth} />
                </TableCell>
                <TableCell sx={{ py: 1, width: 56 }} align="center">
                  <Tooltip title={hidden ? "Unhide party" : "Hide party"}>
                    <IconButton
                      size="small"
                      aria-label={hidden ? "Unhide party" : "Hide party"}
                      onClick={(e) => {
                        e.stopPropagation();
                        onToggleHidden(party.party_id);
                      }}
                    >
                      {hidden ? (
                        <VisibilityOffIcon sx={{ fontSize: 18 }} />
                      ) : (
                        <VisibilityIcon sx={{ fontSize: 18 }} />
                      )}
                    </IconButton>
                  </Tooltip>
                </TableCell>
                <TableCell sx={{ py: 1 }} aria-hidden />
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
    </Box>
  );
};
