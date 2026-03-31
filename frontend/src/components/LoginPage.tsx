import { Box, Button, Typography, useTheme } from "@mui/material";
import LoginIcon from "@mui/icons-material/Login";

import BitSafeLogoDark from "../assets/bitsafe-logo-dark.svg";
import BitSafeLogoLight from "../assets/bitsafe-logo-light.svg";

interface LoginPageProps {
  onLogin: () => void;
  error?: string | null;
}

export const LoginPage = ({ onLogin, error }: LoginPageProps) => {
  const theme = useTheme();
  const logo =
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
      <img src={logo} alt="BitSafe" style={{ height: 40 }} />
      <Typography variant="body1" color="text.secondary">
        Decentralized Party Manager
      </Typography>

      {error && (
        <Typography
          variant="body2"
          color="error"
          sx={{ maxWidth: 400, textAlign: "center" }}
        >
          {error}
        </Typography>
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
  );
};
