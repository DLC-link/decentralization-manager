import { Box, Chip, Tooltip, Typography } from "@mui/material";
import { CopyableText } from "./CopyableText";
import { PartyIdText } from "./PartyIdText";
import { RowCard } from "./RowCard";
import { PaginationControls } from "./Pagination";
import { usePagination } from "../usePagination";
import { EXPANDER_SLOT, columnSx, legendSx } from "../styles";
import type { ExternalPartyInfo } from "../types";

interface ExternalPartyListProps {
  parties: ExternalPartyInfo[];
}

// Shared by the legend and the cards so the columns line up.
const HOSTS_SLOT = 72;
const CONFIRMATIONS_SLOT = 108;
// Wide enough for "YYYY-MM-DD HH:MM UTC" on one line at the mono fact size.
const CREATED_SLOT = 160;

const factSx = {
  fontFamily: "var(--font-mono)",
  fontSize: 13,
  color: "text.secondary",
  // Facts are fixed-width columns; wrapping one would make its card taller
  // than the rest of the list.
  whiteSpace: "nowrap" as const,
};

/**
 * The participants named by the party's hosting mapping. `hosts` is ordered as
 * the mapping lists them, and the confirmation threshold applies across the set
 * — so this is the list any M of which must confirm.
 */
const HostsPanel = ({ party }: { party: ExternalPartyInfo }) => (
  <Box
    sx={{
      pl: `calc(16px + ${EXPANDER_SLOT}px + 16px)`,
      pr: "16px",
      pb: 1.5,
      pt: 0.5,
      display: "flex",
      flexDirection: "column",
      gap: 0.75,
    }}
  >
    <Typography component="span" sx={{ ...legendSx, fontSize: "0.65rem" }}>
      Hosted by
    </Typography>
    {party.hosts.length === 0 ? (
      <Typography variant="body2" color="text.secondary">
        The topology mapping names no participants.
      </Typography>
    ) : (
      party.hosts.map((host) => (
        <Box
          key={host.participant_uid}
          sx={{ display: "flex", alignItems: "center", gap: 1.5, minWidth: 0 }}
        >
          <Box sx={{ flex: 1, minWidth: 0 }}>
            <CopyableText
              text={host.participant_uid}
              truncate={{ start: 32, end: 16 }}
              variant="body2"
            />
          </Box>
          {/* No colour by permission: Confirmation is the norm for a hosting
            * mapping, so tinting it would paint nearly every row green and
            * leave nothing for the exceptions to stand out against. */}
          <Chip label={host.permission} size="small" sx={{ flexShrink: 0 }} />
        </Box>
      ))
    )}
  </Box>
);

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
          {/* Matches the chevron slot each row now carries, so the Party ID
            * legend still sits over the party ids. */}
          <Box sx={{ width: EXPANDER_SLOT, flexShrink: 0 }} aria-hidden />
          <Typography
            component="span"
            sx={{ ...legendSx, flex: 1, minWidth: 0 }}
          >
            Party ID
          </Typography>
          <Tooltip title="How many participants host this party. Any one of them being down does not take the party down.">
            <Typography
              component="span"
              sx={{
                ...legendSx,
                width: HOSTS_SLOT,
                textAlign: "right",
                flexShrink: 0,
                cursor: "help",
              }}
            >
              Hosts
            </Typography>
          </Tooltip>
          <Tooltip title="How many of the hosting participants must confirm a transaction involving this party. Separate from the party's signing threshold — one wallet-held key authorizes, this many hosts confirm.">
            <Typography
              component="span"
              sx={{
                ...legendSx,
                width: CONFIRMATIONS_SLOT,
                textAlign: "right",
                flexShrink: 0,
                cursor: "help",
              }}
            >
              Confirmations
            </Typography>
          </Tooltip>
          <Tooltip title="When the hosting mapping became effective in the synchronizer's topology.">
            <Typography
              component="span"
              sx={{
                ...legendSx,
                width: CREATED_SLOT,
                textAlign: "right",
                flexShrink: 0,
                cursor: "help",
              }}
            >
              Created
            </Typography>
          </Tooltip>
        </Box>

        <Box sx={{ display: "flex", flexDirection: "column", gap: 1 }}>
          {pageItems.map((party) => (
            <RowCard
              key={party.party_id}
              detail={<HostsPanel party={party} />}
              detailLabel={`Show the participants hosting ${party.party_id}`}
              dataAttrs={{ "data-testid": "external-party-row" }}
            >
              <PartyIdText partyId={party.party_id} />
              <Typography
                component="span"
                sx={{
                  ...factSx,
                  width: HOSTS_SLOT,
                  textAlign: "right",
                  flexShrink: 0,
                }}
              >
                {party.host_count}
              </Typography>
              <Typography
                component="span"
                sx={{
                  ...factSx,
                  width: CONFIRMATIONS_SLOT,
                  textAlign: "right",
                  flexShrink: 0,
                }}
              >
                {party.threshold} of {party.host_count}
              </Typography>
              <Typography
                component="span"
                sx={{
                  ...factSx,
                  fontSize: 12,
                  color: "text.disabled",
                  width: CREATED_SLOT,
                  textAlign: "right",
                  flexShrink: 0,
                }}
              >
                {formatCreated(party.created_at)}
              </Typography>
            </RowCard>
          ))}
        </Box>
      </Box>

      {/* Outside the column, so its rule runs the full width of the view. */}
      <PaginationControls
        page={page}
        pageCount={pageCount}
        total={total}
        onChange={setPage}
      />
    </Box>
  );
};
