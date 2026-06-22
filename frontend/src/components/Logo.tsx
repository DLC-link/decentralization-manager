import { Box, Typography, useTheme } from "@mui/material";

import BitSafeLogoB from "../assets/bitsafe-logo-b.svg";
import BitSafeLogoDark from "../assets/bitsafe-logo-dark.svg";
import BitSafeLogoLight from "../assets/bitsafe-logo-light.svg";

import { BITSAFE_BRANDING } from "../constants";

interface LogoProps {
  subtitle?: string;
}

export const Logo = ({ subtitle = "Decentralization Manager" }: LogoProps) => {
  const theme = useTheme();
  const wordmark =
    theme.palette.mode === "light" ? BitSafeLogoDark : BitSafeLogoLight;

  if (!BITSAFE_BRANDING) {
    // Co-brand mode: replace the "itsafe" wordmark with "Decentralization
    // Manager" as the app name. Keep the orange "B" mark on its left.
    return (
      <Box sx={{ display: "flex", alignItems: "center", gap: 1.25 }}>
        <img
          src={BitSafeLogoB}
          alt=""
          onClick={() => window.location.reload()}
          style={{ height: 28, cursor: "pointer", flexShrink: 0 }}
        />
        <Typography
          variant="h6"
          sx={{ fontWeight: 600, lineHeight: 1.15, userSelect: "none" }}
        >
          Decentralization Manager
        </Typography>
      </Box>
    );
  }

  return (
    <Box>
      <img
        src={wordmark}
        alt="BitSafe"
        onClick={() => window.location.reload()}
        style={{ height: 28, cursor: "pointer" }}
      />
      <Typography
        variant="body2"
        color="text.secondary"
        sx={{ mt: 0.5, userSelect: "none" }}
      >
        {subtitle}
      </Typography>
    </Box>
  );
};
