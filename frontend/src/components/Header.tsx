import { useRef, useState } from "react";
import { Box, Container, IconButton, Tooltip, Typography, useTheme } from "@mui/material";
import LogoutIcon from "@mui/icons-material/Logout";

import BitSafeLogoDark from "../assets/bitsafe-logo-dark.svg";
import BitSafeLogoLight from "../assets/bitsafe-logo-light.svg";
import { useAuth } from "../contexts";
import { ThemeSwitcher } from "./ThemeSwitcher";

declare const __BUILD_DATE__: string;

export const Header = () => {
  const theme = useTheme();
  const { token, logout } = useAuth();
  const logo =
    theme.palette.mode === "light" ? BitSafeLogoDark : BitSafeLogoLight;
  const [showBuildDate, setShowBuildDate] = useState(false);
  const clickCount = useRef(0);

  const handleSubtitleClick = () => {
    clickCount.current += 1;
    if (clickCount.current >= 10) {
      clickCount.current = 0;
      setShowBuildDate(false);
      window.location.href = "/swagger-ui/";
    } else if (clickCount.current >= 5) {
      setShowBuildDate(true);
    }
  };

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
        <Box>
          <img
            src={logo}
            alt="BitSafe"
            onClick={() => window.location.reload()}
            style={{ height: 28, cursor: "pointer" }}
          />
          <Typography
            variant="body2"
            color="text.secondary"
            onClick={handleSubtitleClick}
            sx={{ cursor: "default", userSelect: "none" }}
          >
            {showBuildDate
              ? `Build date: ${new Date(__BUILD_DATE__).toLocaleString("hu-HU", { year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false })}`
              : "Monitor and manage your decentralized parties"}
          </Typography>
        </Box>
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
