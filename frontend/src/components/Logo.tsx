import { useRef, useState } from "react";
import { Box, Typography, useTheme } from "@mui/material";

import BitSafeLogoDark from "../assets/bitsafe-logo-dark.svg";
import BitSafeLogoLight from "../assets/bitsafe-logo-light.svg";

declare const __BUILD_DATE__: string;

interface LogoProps {
  subtitle?: string;
}

export const Logo = ({ subtitle = "Decentralization Manager" }: LogoProps) => {
  const theme = useTheme();
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
        sx={{ mt: 0.5, cursor: "default", userSelect: "none" }}
      >
        {showBuildDate
          ? `Build: ${new Date(__BUILD_DATE__).toLocaleString("hu-HU", { year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false })}`
          : subtitle}
      </Typography>
    </Box>
  );
};
