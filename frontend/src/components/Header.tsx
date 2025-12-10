import { Box, Container, Typography } from "@mui/material";
import { ThemeSwitcher } from "./ThemeSwitcher";

export const Header = () => {
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
            ? "rgba(248, 250, 252, 0.5)"
            : "rgba(15, 23, 42, 0.5)",
        borderBottom: (theme) =>
          `1px solid ${theme.palette.mode === "light" ? "rgba(226, 232, 240, 0.5)" : "rgba(51, 65, 85, 0.5)"}`,
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
          <Typography variant="h5" color="primary" sx={{ fontWeight: 700 }}>
            Canton Decentralized Party Manager
          </Typography>
          <Typography variant="body2" color="text.secondary">
            Monitor and manage your decentralized parties
          </Typography>
        </Box>
        <ThemeSwitcher />
      </Container>
    </Box>
  );
}
