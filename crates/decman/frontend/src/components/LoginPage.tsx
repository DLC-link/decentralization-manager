import { Box, Button, Typography, useTheme } from "@mui/material";
import LoginIcon from "@mui/icons-material/Login";

import BitSafeLogoB from "../assets/bitsafe-logo-b.svg";
import BitSafeLogoDark from "../assets/bitsafe-logo-dark.svg";
import BitSafeLogoLight from "../assets/bitsafe-logo-light.svg";

import { BITSAFE_BRANDING } from "../constants";

interface LoginPageProps {
  onLogin: () => void;
}

export const LoginPage = ({ onLogin }: LoginPageProps) => {
  const theme = useTheme();
  const wordmark =
    theme.palette.mode === "light" ? BitSafeLogoDark : BitSafeLogoLight;

  return (
    <Box
      sx={{
        display: "flex",
        flexDirection: "column",
        justifyContent: "center",
        alignItems: "center",
        height: "100vh",
        gap: 3,
        backgroundColor: "background.default",
      }}
    >
      {BITSAFE_BRANDING ? (
        <>
          <img src={wordmark} alt="BitSafe" style={{ height: 40 }} />
          <Typography variant="body1" color="text.secondary">
            Decentralized Party Manager
          </Typography>
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
          <img
            src={wordmark}
            alt="BitSafe"
            style={{ height: 22, opacity: 0.85 }}
          />
        </Box>
      )}
    </Box>
  );
};
