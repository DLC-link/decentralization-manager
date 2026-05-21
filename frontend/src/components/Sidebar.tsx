import {
  Badge,
  Box,
  IconButton,
  List,
  ListItemButton,
  ListItemIcon,
  ListItemText,
  Tooltip,
  useTheme,
} from "@mui/material";
import GroupsIcon from "@mui/icons-material/Groups";
import Inventory2Icon from "@mui/icons-material/Inventory2";
import NotificationsIcon from "@mui/icons-material/Notifications";
import SettingsIcon from "@mui/icons-material/Settings";
import LogoutIcon from "@mui/icons-material/Logout";

import { useAuth } from "../contexts";
import { Logo } from "./Logo";
import { ThemeSwitcher } from "./ThemeSwitcher";

export const SIDEBAR_WIDTH = 260;

interface SidebarProps {
  activeTab: number;
  onTabChange: (tab: number) => void;
  partyCount: number;
  packageCount: number;
  notificationCount: number;
}

const navItems = [
  { label: "Parties", icon: <GroupsIcon />, index: 0 },
  { label: "Packages", icon: <Inventory2Icon />, index: 1 },
  { label: "Configuration", icon: <SettingsIcon />, index: 2 },
  { label: "Pending approvals", icon: <NotificationsIcon />, index: 3 },
];

export const Sidebar = ({
  activeTab,
  onTabChange,
  partyCount,
  packageCount,
  notificationCount,
}: SidebarProps) => {
  const theme = useTheme();
  const { token, logout } = useAuth();

  return (
    <Box
      sx={{
        position: "fixed",
        top: 0,
        left: 0,
        bottom: 0,
        width: SIDEBAR_WIDTH,
        display: "flex",
        flexDirection: "column",
        borderRight: `1px solid ${theme.palette.mode === "light" ? "rgba(224, 224, 224, 0.5)" : "rgba(58, 58, 58, 0.5)"}`,
        backgroundColor: theme.palette.background.paper,
        zIndex: 1100,
      }}
    >
      {/* Logo */}
      <Box sx={{ px: 3, pt: 3, pb: 1 }}>
        <Logo />
      </Box>

      {/* Navigation */}
      <List sx={{ flex: 1, px: 1.5, pt: 2 }}>
        {navItems.map((item) => (
          <ListItemButton
            key={item.index}
            selected={activeTab === item.index}
            onClick={() => onTabChange(item.index)}
            sx={{
              borderRadius: 1.5,
              mb: 0.5,
              "&.Mui-selected": {
                backgroundColor: "primary.main",
                color: "white",
                "& .MuiListItemIcon-root": {
                  color: "white",
                },
                "&:hover": {
                  backgroundColor: "primary.dark",
                },
              },
            }}
          >
            <ListItemIcon sx={{ minWidth: 40 }}>
              {item.icon}
            </ListItemIcon>
            <ListItemText primary={item.label} />
            {item.index === 0 && partyCount > 0 && (
              <Badge
                badgeContent={partyCount}
                color={activeTab === 0 ? "secondary" : "primary"}
                sx={{ mr: 1 }}
              />
            )}
            {item.index === 1 && packageCount > 0 && (
              <Badge
                badgeContent={packageCount}
                color={activeTab === 1 ? "secondary" : "primary"}
                sx={{ mr: 1 }}
              />
            )}
            {item.index === 3 && notificationCount > 0 && (
              <Badge
                badgeContent={notificationCount}
                color={activeTab === 3 ? "secondary" : "error"}
                sx={{ mr: 1 }}
              />
            )}
          </ListItemButton>
        ))}
      </List>

      {/* Bottom: Theme switcher + Logout */}
      <Box
        sx={{
          p: 2,
          borderTop: `1px solid ${theme.palette.divider}`,
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
        }}
      >
        <ThemeSwitcher />
        {token && (
          <Tooltip title="Log out" arrow>
            <IconButton size="small" onClick={logout} color="inherit">
              <LogoutIcon fontSize="small" />
            </IconButton>
          </Tooltip>
        )}
      </Box>
    </Box>
  );
};
