import { Box, Button, Typography, useTheme } from "@mui/material";
import LoginIcon from "@mui/icons-material/Login";

import BitSafeLogoB from "../assets/bitsafe-logo-b.svg";
import BitSafeLogoDark from "../assets/bitsafe-logo-dark.svg";
import BitSafeLogoLight from "../assets/bitsafe-logo-light.svg";

import { BITSAFE_BRANDING } from "../constants";

interface LoginPageProps {
  onLogin: () => void;
}

// Restrained node-cluster motif — small dots joined by thin lines, accent
// tinted and faded back. Mirrors the BitSafe hero treatment: present, never
// busy. Sits behind the content, clipped by the page's overflow.
const NodeMotif = () => (
  <Box
    component="svg"
    viewBox="0 0 360 360"
    aria-hidden="true"
    sx={{
      position: "absolute",
      right: { xs: "-90px", md: "-10px" },
      top: "50%",
      transform: "translateY(-50%)",
      width: { xs: 320, md: 480 },
      height: { xs: 320, md: 480 },
      pointerEvents: "none",
      opacity: 0.4,
      color: "primary.main",
    }}
  >
    <g stroke="currentColor" strokeWidth="0.6" opacity="0.4">
      <line x1="60" y1="70" x2="170" y2="120" />
      <line x1="170" y1="120" x2="120" y2="220" />
      <line x1="170" y1="120" x2="280" y2="90" />
      <line x1="280" y1="90" x2="300" y2="200" />
      <line x1="120" y1="220" x2="220" y2="280" />
      <line x1="300" y1="200" x2="220" y2="280" />
      <line x1="60" y1="70" x2="120" y2="220" />
      <line x1="220" y1="280" x2="170" y2="120" />
    </g>
    <g fill="currentColor">
      <circle cx="60" cy="70" r="2.5" />
      <circle cx="170" cy="120" r="4" />
      <circle cx="280" cy="90" r="2.5" />
      <circle cx="120" cy="220" r="3" />
      <circle cx="300" cy="200" r="2.5" />
      <circle cx="220" cy="280" r="3.5" />
    </g>
  </Box>
);

export const LoginPage = ({ onLogin }: LoginPageProps) => {
  const theme = useTheme();
  const dark = theme.palette.mode === "dark";
  const wordmark = dark ? BitSafeLogoLight : BitSafeLogoDark;

  return (
    <Box
      sx={{
        position: "relative",
        display: "flex",
        flexDirection: "column",
        justifyContent: "center",
        alignItems: "center",
        height: "100vh",
        gap: 3,
        overflow: "hidden",
        backgroundColor: "background.default",
        backgroundImage: dark
          ? "linear-gradient(160deg, #0F0E0D 0%, #1A1614 100%)"
          : "linear-gradient(160deg, #FFFFFF 0%, #FAF9F8 100%)",
      }}
    >
      <NodeMotif />

      <Box
        sx={{
          position: "relative",
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          gap: 2.5,
        }}
      >
        {BITSAFE_BRANDING ? (
          <>
            <img src={wordmark} alt="BitSafe" style={{ height: 40 }} />
            <Typography variant="subtitle2">Decentralized Party Manager</Typography>
          </>
        ) : (
          <>
            <img src={BitSafeLogoB} alt="" style={{ height: 48 }} />
            <Typography variant="h5" sx={{ fontWeight: 600, mt: -1 }}>
              Decentralization Manager
            </Typography>
          </>
        )}

        <Button
          variant="contained"
          size="large"
          startIcon={<LoginIcon />}
          onClick={onLogin}
          sx={{ mt: 1 }}
        >
          Log in
        </Button>
      </Box>

      {!BITSAFE_BRANDING && (
        <Box
          sx={{
            position: "fixed",
            bottom: 24,
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            gap: 0.5,
          }}
        >
          <Typography variant="caption" color="text.secondary">
            Powered by
          </Typography>
          <img src={wordmark} alt="BitSafe" style={{ height: 22, opacity: 0.85 }} />
        </Box>
      )}
    </Box>
  );
};
