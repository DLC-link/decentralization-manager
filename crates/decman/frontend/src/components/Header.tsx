import { Box, Container, IconButton, Tooltip } from "@mui/material";
import LogoutIcon from "@mui/icons-material/Logout";

import { useAuth } from "../contexts";
import { Logo, type BuildInfo } from "./Logo";
import { ThemeSwitcher } from "./ThemeSwitcher";

interface HeaderProps {
  /** Build identity for the header's hidden build-info reveal. */
  buildInfo?: BuildInfo;
}

export const Header = ({ buildInfo }: HeaderProps) => {
  const { token, logout } = useAuth();

  return (
    <Box
      sx={{
        position: "fixed",
        top: 0,
        left: 0,
        right: 0,
        zIndex: 1100,
        backdropFilter: "blur(16px)",
        backgroundColor: (theme) =>
          theme.palette.mode === "light"
            ? "rgba(243, 243, 243, 0.5)"
            : "rgba(26, 26, 26, 0.5)",
        borderBottom: (theme) =>
          `1px solid ${theme.palette.mode === "light" ? "rgba(224, 224, 224, 0.5)" : "rgba(58, 58, 58, 0.5)"}`,
        py: 2.5,
        px: 3,
      }}
    >
      <Container
        maxWidth="md"
        sx={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
        }}
      >
        <Logo
          subtitle="Monitor and manage your decentralized parties"
          buildInfo={buildInfo}
        />
        <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
          <ThemeSwitcher />
          {token && (
            <Tooltip title="Log out" arrow>
              <IconButton size="small" onClick={logout} color="inherit">
                <LogoutIcon fontSize="small" />
              </IconButton>
            </Tooltip>
          )}
        </Box>
      </Container>
    </Box>
  );
};
