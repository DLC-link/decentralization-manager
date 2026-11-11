import type { Dispatch, ReactNode, SetStateAction } from "react";
import {
  Alert,
  Badge,
  Box,
  Container,
  IconButton,
  InputAdornment,
  Tab,
  Tabs,
  TextField,
  Tooltip,
} from "@mui/material";
import FilterListIcon from "@mui/icons-material/FilterList";
import SearchIcon from "@mui/icons-material/Search";
import VisibilityIcon from "@mui/icons-material/Visibility";
import VisibilityOffIcon from "@mui/icons-material/VisibilityOff";
import { Header } from "./Header";
import { LoadingSkeleton } from "./LoadingSkeleton";
import type { PartyFilterControls } from "./partyFilterControls";
import { Sidebar, SIDEBAR_WIDTH, SIDEBAR_WIDTH_COLLAPSED } from "./Sidebar";
import type { DecentralizedParty, NodeConfig } from "../types";

interface AppFrameProps {
  isLargeScreen: boolean;
  activeTab: number;
  navigate: (tab: number) => void;
  parties: DecentralizedParty[];
  packageCount: number;
  notificationCount: number;
  nodeConfig: NodeConfig | null;
  sidebarCollapsed: boolean;
  setSidebarCollapsed: Dispatch<SetStateAction<boolean>>;
  loading: boolean;
  error: string | null;
  setError: (error: string | null) => void;
  showingDetail: boolean;
  showSearchBar: boolean;
  filter: PartyFilterControls;
  children: ReactNode;
}

/**
 * The frame around the tabs: the sidebar or header, the floating parties
 * search bar, the loading and error states, and the mobile tab bar.
 */
export const AppFrame = ({
  isLargeScreen,
  activeTab,
  navigate,
  parties,
  packageCount,
  notificationCount,
  nodeConfig,
  sidebarCollapsed,
  setSidebarCollapsed,
  loading,
  error,
  setError,
  showingDetail,
  showSearchBar,
  filter,
  children,
}: AppFrameProps) => {
  const sidebarWidth = sidebarCollapsed ? SIDEBAR_WIDTH_COLLAPSED : SIDEBAR_WIDTH;
  const {
    partyFilter,
    setPartyFilter,
    refreshParties,
    refreshingParties,
    showHidden,
    setShowHidden,
    hiddenCount,
  } = filter;

  const buildInfo = nodeConfig
    ? {
        version: nodeConfig.version,
        buildVersion: nodeConfig.build_version,
        buildTime: nodeConfig.build_time,
      }
    : undefined;

  return (
    <Box
      sx={{
        minHeight: "100vh",
        backgroundColor: (theme) => theme.palette.background.default,
      }}
    >
      {isLargeScreen ? (
        <Sidebar
          activeTab={activeTab}
          onTabChange={(tab) => navigate(tab)}
          partyCount={parties.length}
          packageCount={packageCount}
          notificationCount={notificationCount}
          buildInfo={buildInfo}
          collapsed={sidebarCollapsed}
          onToggleCollapsed={() => setSidebarCollapsed((c) => !c)}
          network={nodeConfig?.canton.network}
        />
      ) : (
        <Header buildInfo={buildInfo} />
      )}

      {isLargeScreen && activeTab === 0 && !showingDetail && !error && (
        <Box
          sx={{
            position: "fixed",
            top: 16,
            left: `${sidebarWidth}px`,
            right: 0,
            zIndex: 1050,
            display: "flex",
            justifyContent: "center",
            opacity: showSearchBar ? 1 : 0,
            transform: showSearchBar
              ? "translateY(0)"
              : "translateY(-20px)",
            transition: "left 0.15s ease-out, opacity 0.3s ease, transform 0.3s ease",
            pointerEvents: showSearchBar ? "auto" : "none",
          }}
        >
          <Box
            sx={{
              display: "flex",
              alignItems: "center",
              gap: 1,
              backdropFilter: "blur(16px)",
              backgroundColor: (theme) =>
                theme.palette.mode === "light"
                  ? "rgba(255, 255, 255, 0.85)"
                  : "rgba(42, 42, 42, 0.85)",
              borderRadius: 100,
              pl: 0.5,
              pr: 0.5,
              py: 0.5,
              boxShadow: (theme) =>
                theme.palette.mode === "light"
                  ? "0 2px 12px rgba(0,0,0,0.08)"
                  : "0 2px 12px rgba(0,0,0,0.3)",
              border: (theme) =>
                `1px solid ${theme.palette.mode === "light" ? "rgba(0,0,0,0.08)" : "rgba(255,255,255,0.08)"}`,
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
              sx={{
                width: 380,
                "& .MuiOutlinedInput-root": {
                  borderRadius: 100,
                },
                "& .MuiOutlinedInput-notchedOutline": {
                  border: "none",
                },
              }}
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
                >
                  {showHidden ? <VisibilityIcon /> : <VisibilityOffIcon />}
                </IconButton>
              </span>
            </Tooltip>
            <IconButton
              onClick={() => refreshParties(true)}
              disabled={refreshingParties}
              color="primary"
              sx={{
                backgroundColor: "primary.main",
                color: "white",
                "&:hover": {
                  backgroundColor: "primary.dark",
                },
                "&.Mui-disabled": {
                  backgroundColor: "action.disabledBackground",
                  color: "action.disabled",
                },
              }}
            >
              <SearchIcon />
            </IconButton>
          </Box>
        </Box>
      )}

      <Box
        sx={{
          ...(isLargeScreen && { ml: `${sidebarWidth}px`, transition: "margin-left 0.15s ease-out" }),
          ...(activeTab === 1 && {
            height: "100vh",
            overflow: "hidden",
            display: "flex",
            flexDirection: "column",
          }),
          // The parties list and the approvals feed both end in a footer bar
          // that should sit at the bottom of the view even when a short page
          // doesn't fill it — so the region is at least a screen tall and the
          // list flexes into the leftover.
          ...((activeTab === 0 || activeTab === 3) && {
            minHeight: "100vh",
            display: "flex",
            flexDirection: "column",
          }),
        }}
      >
      {loading && (
        <Box sx={{ pt: isLargeScreen ? 4 : 16 }}>
          {isLargeScreen && <Box sx={{ height: 48 }} />}
          <LoadingSkeleton />
        </Box>
      )}

      {!loading && error && (
        <Container maxWidth="md" sx={{ pt: isLargeScreen ? 4 : 16 }}>
          <Alert severity="error" onClose={() => setError(null)}>
            {error}
          </Alert>
        </Container>
      )}

      {!isLargeScreen && !loading && !error && (
        <Box sx={{ pt: 16, px: 2 }}>
          <Tabs
            value={activeTab}
            onChange={(_e, v) => navigate(v)}
            sx={{
              mb: 1,
              borderBottom: 1,
              borderColor: "divider",
              overflow: "visible",
              "& .MuiTabs-scroller": { overflow: "visible !important" },
              "& .MuiTab-root": { overflow: "visible" },
            }}
          >
            <Tab
              label={
                <Badge badgeContent={parties.length} color="primary" sx={{ pr: parties.length ? 2.5 : 0 }}>
                  Parties
                </Badge>
              }
            />
            <Tab
              label={
                <Badge badgeContent={packageCount} color="primary" sx={{ pr: packageCount ? 2.5 : 0 }}>
                  Packages
                </Badge>
              }
            />
            <Tab label="Configuration" />
            <Tab
              label={
                <Badge
                  badgeContent={notificationCount}
                  color={activeTab === 3 ? "primary" : "error"}
                  sx={{ pr: notificationCount ? 2.5 : 0 }}
                >
                  Approvals
                </Badge>
              }
            />
          </Tabs>
        </Box>
      )}
      {children}
      </Box>
    </Box>
  );
};
