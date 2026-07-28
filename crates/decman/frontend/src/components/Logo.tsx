import { useRef, useState } from "react";
import { Box, Typography, useTheme } from "@mui/material";

import BitSafeLogoB from "../assets/bitsafe-logo-b.svg";
import BitSafeLogoDark from "../assets/bitsafe-logo-dark.svg";
import BitSafeLogoLight from "../assets/bitsafe-logo-light.svg";

import { BITSAFE_BRANDING } from "../constants";

declare const __BUILD_DATE__: string;

export interface BuildInfo {
  /** Cargo semver (compatibility version). */
  version?: string;
  /** Display build identity: image tag / short SHA / `<semver>-dev`. */
  buildVersion?: string;
  /** Image build time (RFC 3339), if CI stamped it. */
  buildTime?: string;
}

interface LogoProps {
  subtitle?: string;
  /** Build identity for the hidden build-info reveal. Absent before login. */
  buildInfo?: BuildInfo;
}

const formatTimestamp = (iso: string) =>
  new Date(iso).toLocaleString("hu-HU", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });

export const Logo = ({
  subtitle = "Decentralization Manager",
  buildInfo,
}: LogoProps) => {
  const theme = useTheme();
  const wordmark =
    theme.palette.mode === "light" ? BitSafeLogoDark : BitSafeLogoLight;
  const [showBuildInfo, setShowBuildInfo] = useState(false);
  const clickCount = useRef(0);

  const handleSubtitleClick = () => {
    clickCount.current += 1;
    if (clickCount.current >= 10) {
      clickCount.current = 0;
      setShowBuildInfo(false);
      window.location.href = "/swagger-ui/";
    } else if (clickCount.current >= 5) {
      setShowBuildInfo(true);
    }
  };

  // Prefer the CI-stamped image build time (matches the running image); fall
  // back to the frontend bundle's build date when the backend didn't stamp one
  // (e.g. before login, or a local build).
  const builtAt = buildInfo?.buildTime ?? __BUILD_DATE__;
  const buildLabel = buildInfo?.buildVersion
    ? `Build ${buildInfo.buildVersion}${
        buildInfo.version ? ` · v${buildInfo.version}` : ""
      } · ${formatTimestamp(builtAt)}`
    : `Build: ${formatTimestamp(builtAt)}`;

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
          onClick={handleSubtitleClick}
          sx={{
            fontWeight: 600,
            lineHeight: 1.15,
            cursor: "default",
            userSelect: "none",
          }}
        >
          {showBuildInfo ? buildLabel : "Decentralization Manager"}
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
        onClick={handleSubtitleClick}
        sx={{ mt: 0.5, cursor: "default", userSelect: "none" }}
      >
        {showBuildInfo ? buildLabel : subtitle}
      </Typography>
    </Box>
  );
};
