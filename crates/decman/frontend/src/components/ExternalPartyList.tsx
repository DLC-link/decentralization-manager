import { Alert, Box, Chip, LinearProgress, Tooltip, Typography } from "@mui/material";
import { CopyableText } from "./CopyableText";
import { PartyIdText } from "./PartyIdText";
import { RowCard } from "./RowCard";
import { PaginationControls } from "./Pagination";
import { usePagination } from "../usePagination";
import { formatAge } from "../formatAge";
import { EXPANDER_SLOT, columnSx, legendSx } from "../styles";
import type { ExternalPartyInfo } from "../types";

interface ExternalPartyListProps {
  parties: ExternalPartyInfo[];
  loading?: boolean;
  error?: string | null;
  fetchedAt?: string | null;
  refreshing?: boolean;
  refreshError?: string | null;
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

/**
 * How old the list is, and why it is old when a later topology scan failed.
 * The server keeps the last good list through a failed scan, so the age is
 * what tells an operator the list may be out of date.
 */
const SnapshotStatus = ({
  fetchedAt,
  refreshing,
  refreshError,
}: Pick<ExternalPartyListProps, "fetchedAt" | "refreshing" | "refreshError">) => (
  <>
    {refreshError && (
      <Alert severity="warning" sx={{ mt: 2 }}>
        The latest topology scan failed, so this list comes from an earlier scan.{" "}
        {refreshError}
      </Alert>
    )}
    {fetchedAt && (
      <Typography
        title={fetchedAt}
        sx={{
          fontFamily: "var(--font-mono)",
          fontSize: "0.68rem",
          color: "text.disabled",
          textAlign: "right",
          px: "16px",
          pt: 1,
        }}
      >
        {formatAge(fetchedAt)}
        {refreshing && " · refreshing"}
      </Typography>
    )}
  </>
);

/** Render an RFC 3339 timestamp as a readable UTC date + time. */
const formatCreated = (iso: string | null | undefined) => {
  if (!iso) return "—";
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "—";
  return date.toISOString().replace("T", " ").slice(0, 16) + " UTC";
};

export const ExternalPartyList = ({
  parties,
  loading,
  error,
  fetchedAt,
  refreshing,
  refreshError,
}: ExternalPartyListProps) => {
  const { page, setPage, pageCount, pageItems, total } = usePagination(parties);

  if (error) {
    return <Alert severity="error" sx={{ mt: 2 }}>{error}</Alert>;
  }
  if (loading) {
    return (
      <Box sx={{ mt: 2 }}>
        <LinearProgress aria-label="Loading external parties" />
        {/* Only the server's first scan sets `refreshing` while still loading.
          * That scan can take minutes, so a bare bar would look stuck. */}
        {refreshing && (
          <Typography
            variant="body2"
            color="text.secondary"
            sx={{ textAlign: "center", pt: 1.5 }}
          >
            This node is reading its topology. The first scan after a restart can take a few minutes.
          </Typography>
        )}
      </Box>
    );
  }

  const status = (
    <SnapshotStatus
      fetchedAt={fetchedAt}
      refreshing={refreshing}
      refreshError={refreshError}
    />
  );

  if (parties.length === 0) {
    return (
      <Box sx={columnSx}>
        {status}
        <Typography
          variant="body2"
          color="text.secondary"
          sx={{ textAlign: "center", py: 6 }}
        >
          No external parties hosted on this node
        </Typography>
      </Box>
    );
  }

  return (
    <Box sx={{ pt: 1, flex: 1, display: "flex", flexDirection: "column" }}>
      <Box sx={{ ...columnSx, flex: 1 }}>
        {status}
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
          <Tooltip title="Live means this node holds the party's contracts and confirms for it. Onboarding means it is assigned here but still carries Canton's onboarding marker, so its contracts have not been replicated yet.">
            <Typography component="span" sx={{ ...legendSx, cursor: "help" }}>
              Status
            </Typography>
          </Tooltip>
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
              {/* Live means this node holds the party's contracts and confirms
                * for it. Onboarding means the party is assigned here but still
                * carries Canton's onboarding marker, so it holds none of them
                * and confirms nothing — listing that as a normal row is how an
                * operator concludes a replication finished when it has not. */}
              <Chip
                label={party.onboarding ? "Onboarding" : "Live"}
                size="small"
                color={party.onboarding ? "warning" : "success"}
                variant="outlined"
                sx={{ flexShrink: 0 }}
              />
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
