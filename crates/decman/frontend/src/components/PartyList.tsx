import { Box, Chip, IconButton, Tooltip, Typography } from "@mui/material";
import type { ChipProps } from "@mui/material";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import ErrorIcon from "@mui/icons-material/Error";
import ScienceIcon from "@mui/icons-material/Science";
import StarIcon from "@mui/icons-material/Star";
import StarBorderIcon from "@mui/icons-material/StarBorder";
import VisibilityIcon from "@mui/icons-material/Visibility";
import VisibilityOffIcon from "@mui/icons-material/VisibilityOff";
import { PartyIdText } from "./PartyIdText";
import { RowCard } from "./RowCard";
import { PaginationControls } from "./Pagination";
import { usePagination } from "../usePagination";
import {
  AUTH_SLOT,
  STAR_SLOT,
  VISIBILITY_SLOT,
  columnSx,
  fabGutterSx,
  legendSx,
} from "../styles";
import type {
  DecentralizedParty,
  FormerParty,
  PartyAuthStatus,
  PurgeStatus,
} from "../types";

interface PartyListProps {
  parties: DecentralizedParty[];
  authStatuses: PartyAuthStatus[];
  onSelectParty: (partyId: string) => void;
  isHidden: (partyId: string) => boolean;
  onToggleHidden: (partyId: string) => void;
  /** Starred parties; the caller sorts them to the top. */
  isFavorite: (partyId: string) => boolean;
  onToggleFavorite: (partyId: string) => void;
  /** Parties this node no longer hosts. Listed after the hosted ones, unpaged;
   * selecting one calls `onSelectParty` too. */
  formerParties?: FormerParty[];
}

const PURGE_CHIP: Record<
  PurgeStatus,
  { label: string; color: ChipProps["color"]; variant?: ChipProps["variant"] }
> = {
  none: { label: "Removed", color: "warning" },
  running: { label: "Purging…", color: "info" },
  failed: { label: "Purge failed", color: "error" },
  succeeded: { label: "Purged", color: "success", variant: "outlined" },
};

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
  isFavorite,
  onToggleFavorite,
  formerParties = [],
}: PartyListProps) => {
  const { page, setPage, pageCount, pageItems, total } = usePagination(parties);

  if (parties.length === 0 && formerParties.length === 0) {
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
              width: STAR_SLOT,
              textAlign: "right",
              flexShrink: 0,
            }}
          >
            Star
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

        {parties.length === 0 && (
          <Typography
            variant="body2"
            color="text.secondary"
            sx={{ textAlign: "center", py: 3 }}
          >
            No parties found
          </Typography>
        )}

        <Box sx={{ display: "flex", flexDirection: "column", gap: 1 }}>
          {pageItems.map((party) => {
            const auth = authStatuses.find(
              (a) => a.dec_party_id === party.party_id,
            );
            const hidden = isHidden(party.party_id);
            const starred = isFavorite(party.party_id);
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
                    width: STAR_SLOT,
                    flexShrink: 0,
                    display: "flex",
                    justifyContent: "flex-end",
                  }}
                >
                  <Tooltip title={starred ? "Unstar party" : "Star party"}>
                    <IconButton
                      size="small"
                      aria-label="Star party"
                      aria-pressed={starred}
                      onClick={(e) => {
                        // Starring must not open the party.
                        e.stopPropagation();
                        onToggleFavorite(party.party_id);
                      }}
                    >
                      {starred ? (
                        <StarIcon sx={{ fontSize: 18, color: "warning.main" }} />
                      ) : (
                        <StarBorderIcon sx={{ fontSize: 18 }} />
                      )}
                    </IconButton>
                  </Tooltip>
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

        {formerParties.length > 0 && (
          <Box sx={{ mt: 3, mb: 2 }}>
            <Typography
              component="p"
              sx={{ ...legendSx, px: "16px", pb: 1 }}
            >
              No longer hosted by this node
            </Typography>
            <Box sx={{ display: "flex", flexDirection: "column", gap: 1 }}>
              {formerParties.map((party) => {
                const chip = PURGE_CHIP[party.purge_status];
                return (
                  <RowCard
                    key={party.party_id}
                    onActivate={() => onSelectParty(party.party_id)}
                    tone="warning"
                    dimmed={party.purge_status === "succeeded"}
                    ariaLabel={`Open former party ${party.party_id}`}
                    dataAttrs={{ "data-testid": "former-party-row" }}
                  >
                    <PartyIdText partyId={party.party_id} />
                    <Box sx={{ width: AUTH_SLOT, flexShrink: 0 }} />
                    {/* Spans the star and visibility columns and the gap
                      * between them, so the chip ends where they do. */}
                    <Box
                      sx={{
                        width: STAR_SLOT + 16 + VISIBILITY_SLOT,
                        flexShrink: 0,
                        display: "flex",
                        justifyContent: "flex-end",
                      }}
                    >
                      <Chip
                        size="small"
                        label={chip.label}
                        color={chip.color}
                        variant={chip.variant}
                      />
                    </Box>
                    <Box sx={fabGutterSx} aria-hidden />
                  </RowCard>
                );
              })}
            </Box>
          </Box>
        )}
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
