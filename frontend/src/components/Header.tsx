import { Box, Container, Typography, useTheme } from "@mui/material";

import BitSafeLogoDark from "../assets/bitsafe-logo-dark.svg";
import BitSafeLogoLight from "../assets/bitsafe-logo-light.svg";
import { ThemeSwitcher } from "./ThemeSwitcher";

export const Header = () => {
  const theme = useTheme();
  const logo = theme.palette.mode === "light" ? BitSafeLogoDark : BitSafeLogoLight;

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
          <Typography variant="body2" color="text.secondary">
            Monitor and manage your decentralized parties
          </Typography>
        </Box>
        <ThemeSwitcher />
      </Container>
    </Box>
  );
};
