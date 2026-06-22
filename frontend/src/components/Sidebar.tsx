import {
  Badge,
  Box,
  IconButton,
  List,
  ListItemButton,
  ListItemIcon,
  ListItemText,
  Tooltip,
  Typography,
  useTheme,
} from "@mui/material";
import GroupsIcon from "@mui/icons-material/Groups";
import Inventory2Icon from "@mui/icons-material/Inventory2";
import NotificationsIcon from "@mui/icons-material/Notifications";
import SettingsIcon from "@mui/icons-material/Settings";
import LogoutIcon from "@mui/icons-material/Logout";
import ChevronLeftIcon from "@mui/icons-material/ChevronLeft";
import ChevronRightIcon from "@mui/icons-material/ChevronRight";

import BitSafeLogoB from "../assets/bitsafe-logo-b.svg";
import BitSafeLogoDark from "../assets/bitsafe-logo-dark.svg";
import BitSafeLogoLight from "../assets/bitsafe-logo-light.svg";

import { useAuth } from "../contexts";
import { BITSAFE_BRANDING } from "../constants";
import type { Network } from "../types";
import { Logo } from "./Logo";
import { ThemeSwitcher } from "./ThemeSwitcher";

export const SIDEBAR_WIDTH = 260;
export const SIDEBAR_WIDTH_COLLAPSED = 56;

interface SidebarProps {
  activeTab: number;
  onTabChange: (tab: number) => void;
  partyCount: number;
  packageCount: number;
  notificationCount: number;
  collapsed: boolean;
  onToggleCollapsed: () => void;
  /** Canton network the node is connected to (devnet / testnet / mainnet). */
  network?: Network;
}

// Network indicator colors — mainnet stands out (production), testnet warns,
// devnet is informational.
const NETWORK_COLOR: Record<Network, string> = {
  mainnet: "success.main",
  testnet: "warning.main",
  devnet: "info.main",
};

const navItems = [
  { label: "Parties", icon: <GroupsIcon />, index: 0 },
  { label: "Packages", icon: <Inventory2Icon />, index: 1 },
  { label: "Configuration", icon: <SettingsIcon />, index: 2 },
  { label: "Pending approvals", icon: <NotificationsIcon />, index: 3 },
];

// Compile-time build metadata (injected via vite `define`). Shown at the
// bottom of the expanded sidebar — see vite.config.ts / build.rs.
const BUILD_INFO = (() => {
  const version = __APP_VERSION__ === "dev" ? "dev build" : `v${__APP_VERSION__}`;
  let when = __BUILD_DATE__;
  try {
    const d = new Date(__BUILD_DATE__);
    const p = (n: number) => String(n).padStart(2, "0");
    when = `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
  } catch {
    /* fall back to the raw ISO string */
  }
  return `${version} · ${when}`;
})();

export const Sidebar = ({
  activeTab,
  onTabChange,
  partyCount,
  packageCount,
  notificationCount,
  collapsed,
  onToggleCollapsed,
  network,
}: SidebarProps) => {
  const theme = useTheme();
  const { token, logout } = useAuth();
  const poweredByLogo =
    theme.palette.mode === "light" ? BitSafeLogoDark : BitSafeLogoLight;

  const countFor = (index: number) =>
    index === 0 ? partyCount : index === 1 ? packageCount : index === 3 ? notificationCount : 0;
  // A selected nav item has an accent background, so its badge flips to
  // `secondary` (white on dark) for contrast. Unselected: notifications red,
  // the rest accent.
  const badgeColor = (index: number): "primary" | "secondary" | "error" =>
    activeTab === index ? "secondary" : index === 3 ? "error" : "primary";

  return (
    <Box
      sx={{
        position: "fixed",
        top: 0,
        left: 0,
        bottom: 0,
        width: collapsed ? SIDEBAR_WIDTH_COLLAPSED : SIDEBAR_WIDTH,
        transition: "width 0.15s ease-out",
        display: "flex",
        flexDirection: "column",
        borderRight: `1px solid ${theme.palette.divider}`,
        backgroundColor: theme.palette.background.paper,
        zIndex: 1100,
        overflowX: "hidden",
      }}
    >
      {/* Logo + collapse toggle */}
      <Box
        sx={{
          display: "flex",
          flexDirection: collapsed ? "column" : "row",
          alignItems: collapsed ? "center" : "flex-start",
          justifyContent: "space-between",
          gap: 1,
          px: collapsed ? 1 : 3,
          pt: 3,
          pb: 1,
        }}
      >
        {collapsed ? (
          <img
            src={BitSafeLogoB}
            alt="BitSafe"
            onClick={() => window.location.reload()}
            style={{ height: 28, cursor: "pointer" }}
          />
        ) : (
          <Logo />
        )}
        <Tooltip
          title={collapsed ? "Expand sidebar" : "Collapse sidebar"}
          placement="right"
          arrow
        >
          <IconButton size="small" onClick={onToggleCollapsed} sx={{ mt: collapsed ? 0.5 : -0.5 }}>
            {collapsed ? <ChevronRightIcon fontSize="small" /> : <ChevronLeftIcon fontSize="small" />}
          </IconButton>
        </Tooltip>
      </Box>

      {/* Network indicator — which Canton network the node is connected to */}
      {network &&
        (collapsed ? (
          <Tooltip title={`Network: ${network}`} placement="right" arrow>
            <Box sx={{ display: "flex", justifyContent: "center", px: 1, pb: 0.5 }}>
              <Box
                sx={{
                  width: 8,
                  height: 8,
                  borderRadius: "50%",
                  bgcolor: NETWORK_COLOR[network] ?? "text.disabled",
                }}
              />
            </Box>
          </Tooltip>
        ) : (
          <Box sx={{ px: 3, pb: 0.5 }}>
            <Box
              sx={{
                display: "inline-flex",
                alignItems: "center",
                gap: 0.75,
                px: 1,
                py: 0.25,
                borderRadius: 1,
                border: 1,
                borderColor: "divider",
              }}
            >
              <Box
                sx={{
                  width: 7,
                  height: 7,
                  borderRadius: "50%",
                  bgcolor: NETWORK_COLOR[network] ?? "text.disabled",
                }}
              />
              <Typography
                sx={{
                  fontFamily: "var(--font-mono)",
                  fontSize: "0.62rem",
                  letterSpacing: "0.12em",
                  textTransform: "uppercase",
                  color: "text.secondary",
                }}
              >
                {network}
              </Typography>
            </Box>
          </Box>
        ))}

      {/* Navigation */}
      <List sx={{ flex: 1, px: collapsed ? 1 : 1.5, pt: 2 }}>
        {navItems.map((item) => {
          const count = countFor(item.index);
          const button = (
            <ListItemButton
              key={item.index}
              selected={activeTab === item.index}
              onClick={() => onTabChange(item.index)}
              sx={{
                borderRadius: 1.5,
                mb: 0.5,
                px: collapsed ? 1 : 2,
                justifyContent: collapsed ? "center" : "flex-start",
                "&.Mui-selected": {
                  backgroundColor: "primary.main",
                  color: "white",
                  "& .MuiListItemIcon-root": { color: "white" },
                  "&:hover": { backgroundColor: "primary.dark" },
                },
              }}
            >
              <ListItemIcon sx={{ minWidth: collapsed ? 0 : 40, justifyContent: "center" }}>
                {collapsed && count > 0 ? (
                  <Badge badgeContent={count} color={badgeColor(item.index)} overlap="circular">
                    {item.icon}
                  </Badge>
                ) : (
                  item.icon
                )}
              </ListItemIcon>
              {!collapsed && <ListItemText primary={item.label} />}
              {!collapsed && count > 0 && (
                <Badge badgeContent={count} color={badgeColor(item.index)} sx={{ mr: 1 }} />
              )}
            </ListItemButton>
          );
          return collapsed ? (
            <Tooltip key={item.index} title={item.label} placement="right" arrow>
              {button}
            </Tooltip>
          ) : (
            button
          );
        })}
      </List>

      {/* Powered-by footer (co-brand only, expanded only) */}
      {!BITSAFE_BRANDING && !collapsed && (
        <Box
          sx={{
            px: 2,
            pt: 1.5,
            pb: 1,
            borderTop: `1px solid ${theme.palette.divider}`,
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            gap: 0.5,
          }}
        >
          <Typography variant="caption" color="text.secondary" sx={{ lineHeight: 1 }}>
            Powered by
          </Typography>
          <img src={poweredByLogo} alt="BitSafe" style={{ height: 18, opacity: 0.85 }} />
        </Box>
      )}

      {/* Build version + date — always shown when expanded */}
      {!collapsed && (
        <Box
          sx={{
            px: 2,
            pt: 1,
            ...(BITSAFE_BRANDING && { borderTop: `1px solid ${theme.palette.divider}` }),
          }}
        >
          <Typography
            variant="caption"
            color="text.secondary"
            sx={{ fontFamily: "var(--font-mono)", fontSize: "0.66rem", letterSpacing: "0.02em" }}
          >
            {BUILD_INFO}
          </Typography>
        </Box>
      )}

      {/* Theme switcher + Logout */}
      <Box
        sx={{
          p: collapsed ? 1 : 2,
          pt: collapsed ? 1 : 1.5,
          display: "flex",
          flexDirection: collapsed ? "column" : "row",
          alignItems: "center",
          justifyContent: collapsed ? "center" : "space-between",
          gap: 1,
        }}
      >
        {!collapsed && <ThemeSwitcher />}
        {token && (
          <Tooltip title="Log out" placement="right" arrow>
            <IconButton size="small" onClick={logout} color="inherit">
              <LogoutIcon fontSize="small" />
            </IconButton>
          </Tooltip>
        )}
      </Box>
    </Box>
  );
};
