import { Box, IconButton, Tooltip, Typography } from "@mui/material";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import ErrorIcon from "@mui/icons-material/Error";
import ScienceIcon from "@mui/icons-material/Science";
import VisibilityIcon from "@mui/icons-material/Visibility";
import VisibilityOffIcon from "@mui/icons-material/VisibilityOff";
import { PartyIdText } from "./PartyIdText";
import { RowCard } from "./RowCard";
import { PaginationControls } from "./Pagination";
import { usePagination } from "../usePagination";
import {
  AUTH_SLOT,
  VISIBILITY_SLOT,
  columnSx,
  fabGutterSx,
  legendSx,
} from "../styles";
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
  const { page, setPage, pageCount, pageItems, total } = usePagination(parties);

  if (parties.length === 0) {
    return (
      <Typography
        variant="body2"
        color="text.secondary"
        sx={{ textAlign: "center", py: 6 }}
      >
        No parties found
      </Typography>
    );
  }

  return (
    <Box sx={{ pt: 1, flex: 1, display: "flex", flexDirection: "column" }}>
      <Box sx={{ ...columnSx, flex: 1 }}>
        {/* Legend — padded to line up with the cards' own 16px inset. */}
        <Box
          sx={{
            display: "flex",
            alignItems: "center",
            gap: 2,
            px: "16px",
            pb: 1,
          }}
        >
          <Typography
            component="span"
            sx={{ ...legendSx, flex: 1, minWidth: 0 }}
          >
            Party ID
          </Typography>
          <Typography
            component="span"
            sx={{
              ...legendSx,
              width: AUTH_SLOT,
              textAlign: "right",
              flexShrink: 0,
            }}
          >
            Auth
          </Typography>
          <Typography
            component="span"
            sx={{
              ...legendSx,
              width: VISIBILITY_SLOT,
              textAlign: "right",
              flexShrink: 0,
            }}
          >
            Visibility
          </Typography>
          <Box sx={fabGutterSx} aria-hidden />
        </Box>

        <Box sx={{ display: "flex", flexDirection: "column", gap: 1 }}>
          {pageItems.map((party) => {
            const auth = authStatuses.find(
              (a) => a.dec_party_id === party.party_id,
            );
            const hidden = isHidden(party.party_id);
            return (
              <RowCard
                key={party.party_id}
                onActivate={() => onSelectParty(party.party_id)}
                dimmed={hidden}
                ariaLabel={`Open party ${party.party_id}`}
              >
                <PartyIdText partyId={party.party_id} />
                <Box
                  sx={{
                    width: AUTH_SLOT,
                    flexShrink: 0,
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "flex-end",
                  }}
                >
                  <AuthStatusIcon status={auth} />
                </Box>
                <Box
                  sx={{
                    width: VISIBILITY_SLOT,
                    flexShrink: 0,
                    display: "flex",
                    justifyContent: "flex-end",
                  }}
                >
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
                </Box>
                <Box sx={fabGutterSx} aria-hidden />
              </RowCard>
            );
          })}
        </Box>
      </Box>

      {/* Outside the column: the rule runs the full width of the view, and the
       * contents are centered, so nothing lands under the FAB. */}
      <PaginationControls
        page={page}
        pageCount={pageCount}
        total={total}
        onChange={setPage}
      />
    </Box>
  );
};
