import type { RefObject } from "react";
import {
  Box,
  Chip,
  Fab,
  IconButton,
  InputAdornment,
  LinearProgress,
  Tab,
  Tabs,
  TextField,
  Tooltip,
} from "@mui/material";
import AddIcon from "@mui/icons-material/Add";
import FilterListIcon from "@mui/icons-material/FilterList";
import SearchIcon from "@mui/icons-material/Search";
import VisibilityIcon from "@mui/icons-material/Visibility";
import VisibilityOffIcon from "@mui/icons-material/VisibilityOff";
import { ADMIN_ACCESS } from "../constants";
import { participantName } from "../sharedParties";
import { columnSx } from "../styles";
import type { useExternalParties } from "../useExternalParties";
import { ExternalPartyList } from "./ExternalPartyList";
import { FormerPartyDetail } from "./FormerPartyDetail";
import { PartyDetail } from "./PartyDetail";
import { PartyList } from "./PartyList";
import type { PartyFilterControls } from "./partyFilterControls";
import type {
  DecentralizedParty,
  FormerParty,
  NetworkConfig,
  NodeConfig,
  PartyAuthStatus,
} from "../types";

interface PartiesTabProps {
  filter: PartyFilterControls;
  isLargeScreen: boolean;
  selectedParty: DecentralizedParty | undefined;
  selectedFormerParty: FormerParty | undefined;
  selectedPartyId: string | null;
  setSelectedPartyId: (partyId: string | null) => void;
  showingDetail: boolean;
  backToList: () => void;
  navigate: (tab: number, partySlug?: string | null, sharedScope?: string | null) => void;
  goToNotifications: () => void;
  showSharedParties: (participantId: string) => void;
  reloadFormerParties: () => Promise<void>;
  markPurging: (partyId: string) => void;
  nodeConfig: NodeConfig | null;
  networkConfig: NetworkConfig | null;
  authStatuses: PartyAuthStatus[];
  refreshAuthStatus: () => Promise<void>;
  operatorParty: string;
  partiesView: "decentralized" | "external";
  setPartiesView: (view: "decentralized" | "external") => void;
  sharedWith: string | null;
  setSharedWith: (participantId: string | null) => void;
  visibleParties: DecentralizedParty[];
  visibleFormerParties: FormerParty[];
  externalParties: ReturnType<typeof useExternalParties>;
  isHidden: (partyId: string) => boolean;
  toggleHidden: (partyId: string) => void;
  isFavorite: (partyId: string) => boolean;
  toggleFavorite: (partyId: string) => void;
  /** Where the list was scrolled when a party was opened, for Back to restore. */
  savedScrollYRef: RefObject<number>;
  setOnboardingDialogOpen: (open: boolean) => void;
}

/**
 * The parties tab: the selected party's details, or the list with its
 * filter, the decentralized and external tabs, and the Create Party button.
 */
export const PartiesTab = ({
  filter,
  isLargeScreen,
  selectedParty,
  selectedFormerParty,
  selectedPartyId,
  setSelectedPartyId,
  showingDetail,
  backToList,
  navigate,
  goToNotifications,
  showSharedParties,
  reloadFormerParties,
  markPurging,
  nodeConfig,
  networkConfig,
  authStatuses,
  refreshAuthStatus,
  operatorParty,
  partiesView,
  setPartiesView,
  sharedWith,
  setSharedWith,
  visibleParties,
  visibleFormerParties,
  externalParties,
  isHidden,
  toggleHidden,
  isFavorite,
  toggleFavorite,
  savedScrollYRef,
  setOnboardingDialogOpen,
}: PartiesTabProps) => {
  const {
    partyFilter,
    setPartyFilter,
    refreshParties,
    refreshingParties,
    showHidden,
    setShowHidden,
    hiddenCount,
  } = filter;

  return (
    // Passes the region's leftover height down to the list, so its footer
    // bar can sit at the bottom of the view on a short page.
    <Box
      sx={{
        pt: isLargeScreen ? 4 : 0,
        flex: 1,
        display: "flex",
        flexDirection: "column",
      }}
    >
      {selectedParty ? (
        <PartyDetail
          party={selectedParty}
          onBack={backToList}
          onRefresh={() => refreshParties(true)}
          onNavigateToNotifications={goToNotifications}
          selfParticipantId={nodeConfig?.node.participant_id}
          authStatus={authStatuses.find(
            (a) => a.dec_party_id === selectedPartyId,
          )}
          onAuthRefresh={refreshAuthStatus}
          operatorParty={operatorParty}
          network={nodeConfig?.canton.network}
          onCheckDars={() => navigate(1, selectedPartyId)}
          onOpenPeerConfig={(participantId) => navigate(2, participantId)}
          onShowSharedParties={showSharedParties}
        />
      ) : selectedFormerParty ? (
        <FormerPartyDetail
          party={selectedFormerParty}
          onBack={backToList}
          onChanged={() => void reloadFormerParties()}
          onPurgeStarted={() => markPurging(selectedFormerParty.party_id)}
        />
      ) : (
        <>
          {isLargeScreen ? (
            <Box sx={{ height: 48 }} />
          ) : (
            <Box sx={{ mt: 2, mb: 2, px: 2 }}>
              <Box
                sx={{
                  display: "flex",
                  alignItems: "flex-start",
                  gap: 1,
                }}
              >
                <TextField
                  size="small"
                  placeholder="Filter by full prefix (e.g. cbtc-network)"
                  value={partyFilter}
                  onChange={(e) => setPartyFilter(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      refreshParties(true);
                    }
                  }}
                  disabled={refreshingParties}
                  slotProps={{
                    input: {
                      startAdornment: (
                        <InputAdornment position="start">
                          <FilterListIcon fontSize="small" color="action" />
                        </InputAdornment>
                      ),
                    },
                  }}
                  sx={{ width: 300 }}
                />
                <Tooltip
                  title={
                    showHidden
                      ? `Hide ${hiddenCount} hidden ${hiddenCount === 1 ? "party" : "parties"}`
                      : hiddenCount > 0
                        ? `Show ${hiddenCount} hidden ${hiddenCount === 1 ? "party" : "parties"}`
                        : "No hidden parties"
                  }
                >
                  <span>
                    <IconButton
                      onClick={() => setShowHidden((v) => !v)}
                      disabled={hiddenCount === 0 && !showHidden}
                      color={showHidden ? "primary" : "default"}
                      sx={{ mt: "1px" }}
                    >
                      {showHidden ? <VisibilityIcon /> : <VisibilityOffIcon />}
                    </IconButton>
                  </span>
                </Tooltip>
                <IconButton
                  onClick={() => refreshParties(true)}
                  disabled={refreshingParties}
                  color="primary"
                  sx={{ mt: "1px" }}
                >
                  <SearchIcon />
                </IconButton>
              </Box>
              {refreshingParties && (
                <LinearProgress sx={{ mt: 1, borderRadius: 1 }} />
              )}
            </Box>
          )}

          <Tabs
            value={partiesView}
            onChange={(_e, v) => setPartiesView(v)}
            sx={{ ...columnSx, mb: 1 }}
          >
            <Tab value="decentralized" label="Decentralized Parties" />
            <Tab value="external" label="External Parties" />
          </Tabs>

          {partiesView === "decentralized" && sharedWith && (
            <Box sx={{ ...columnSx, mb: 1 }}>
              <Tooltip title={sharedWith} describeChild>
                <Chip
                  size="small"
                  label={`Shared with ${participantName(sharedWith, networkConfig?.peers)}`}
                  onDelete={() => {
                    // `navigate` keeps the filter for a prefix slug.
                    setSharedWith(null);
                    navigate(0, partyFilter || null);
                  }}
                  data-testid="shared-scope-chip"
                />
              </Tooltip>
            </Box>
          )}

          {partiesView === "decentralized" ? (
            <PartyList
              parties={visibleParties}
              authStatuses={authStatuses}
              onSelectParty={(id) => {
                savedScrollYRef.current = window.scrollY;
                setSelectedPartyId(id);
                navigate(0, id, sharedWith);
                window.scrollTo(0, 0);
              }}
              isHidden={isHidden}
              onToggleHidden={toggleHidden}
              isFavorite={isFavorite}
              onToggleFavorite={toggleFavorite}
              formerParties={visibleFormerParties}
            />
          ) : (
            <ExternalPartyList {...externalParties} />
          )}
        </>
      )}

      {ADMIN_ACCESS && !showingDetail && partiesView === "decentralized" && (
        <Tooltip title="Create Party" arrow>
          <Fab
            color="primary"
            aria-label="Create Party"
            onClick={() => setOnboardingDialogOpen(true)}
            sx={{
              position: "fixed",
              bottom: 24,
              right: 24,
              zIndex: 1101,
            }}
          >
            <AddIcon />
          </Fab>
        </Tooltip>
      )}
    </Box>
  );
};
