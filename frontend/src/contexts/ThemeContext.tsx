import { useState, useEffect, useMemo, type ReactNode } from "react";
import { ThemeProvider as MuiThemeProvider, createTheme } from "@mui/material/styles";
import { CssBaseline } from "@mui/material";
import { ThemeContext, type ThemeMode } from "./ThemeContextValue";

// BitSafe Brand Colors
// Primary: Orange #ff6633
// Secondary: Charcoal #1a1a1a
// Tertiary: White #ffffff
// Accent: Light Gray #f3f3f3

const getDesignTokens = (mode: "light" | "dark") => ({
  palette: {
    mode,
    primary: mode === "light"
      ? {
          main: "#ff6633",
          light: "#ff8559",
          dark: "#e55a2b",
        }
      : {
          main: "#ff8559",
          light: "#ffa37d",
          dark: "#ff6633",
        },
    secondary: mode === "light"
      ? {
          main: "#1a1a1a",
          light: "#333333",
          dark: "#000000",
        }
      : {
          main: "#f3f3f3",
          light: "#ffffff",
          dark: "#e0e0e0",
        },
    success: {
      main: "#10b981",
      light: "#34d399",
      dark: "#059669",
    },
    ...(mode === "light"
      ? {
          background: {
            default: "#f3f3f3",
            paper: "#ffffff",
          },
          text: {
            primary: "#1a1a1a",
            secondary: "#666666",
          },
        }
      : {
          background: {
            default: "#1a1a1a",
            paper: "#2a2a2a",
          },
          text: {
            primary: "#f3f3f3",
            secondary: "#a0a0a0",
          },
        }),
  },
  typography: {
    fontFamily: '"Space Grotesk", -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif',
    h4: {
      fontWeight: 700,
      letterSpacing: "-0.02em",
    },
    h6: {
      fontWeight: 600,
      letterSpacing: "-0.01em",
    },
    subtitle1: {
      fontWeight: 600,
    },
    subtitle2: {
      fontWeight: 600,
      color: mode === "light" ? "#666666" : "#a0a0a0",
      textTransform: "uppercase" as const,
      fontSize: "0.75rem",
      letterSpacing: "0.05em",
    },
  },
  shape: {
    borderRadius: 12,
  },
  components: {
    MuiDialog: {
      defaultProps: {
        disableScrollLock: true,
      },
    },
    MuiCard: {
      styleOverrides: {
        root: {
          boxShadow: mode === "light"
            ? "0 1px 3px 0 rgb(0 0 0 / 0.1), 0 1px 2px -1px rgb(0 0 0 / 0.1)"
            : "0 1px 3px 0 rgb(0 0 0 / 0.3), 0 1px 2px -1px rgb(0 0 0 / 0.3)",
          border: `1px solid ${mode === "light" ? "#e0e0e0" : "#3a3a3a"}`,
        },
      },
    },
    MuiAccordion: {
      styleOverrides: {
        root: {
          boxShadow: mode === "light"
            ? "0 1px 3px 0 rgb(0 0 0 / 0.1), 0 1px 2px -1px rgb(0 0 0 / 0.1)"
            : "0 1px 3px 0 rgb(0 0 0 / 0.3), 0 1px 2px -1px rgb(0 0 0 / 0.3)",
          border: `1px solid ${mode === "light" ? "#e0e0e0" : "#3a3a3a"}`,
          "&:before": {
            display: "none",
          },
          "&.Mui-expanded": {
            marginTop: 0,
            marginBottom: 16,
          },
        },
      },
    },
    MuiAccordionSummary: {
      styleOverrides: {
        root: {
          backgroundColor: mode === "light" ? "#f3f3f3" : "#2a2a2a",
          borderRadius: "12px 12px 0 0",
          flexDirection: "row-reverse" as const,
          gap: 8,
          "&.Mui-expanded": {
            minHeight: 48,
          },
        },
        content: {
          "&.Mui-expanded": {
            margin: "12px 0",
          },
        },
        expandIconWrapper: {
          marginRight: 0,
          marginLeft: 0,
        },
      },
    },
    MuiChip: {
      styleOverrides: {
        root: {
          fontWeight: 500,
          borderRadius: 8,
        },
        colorDefault: {
          backgroundColor: mode === "light" ? "#e8e8e8" : "#3a3a3a",
          color: mode === "light" ? "#4a4a4a" : "#d0d0d0",
        },
        colorPrimary: {
          backgroundColor: "#ff6633",
        },
        colorSuccess: {
          backgroundColor: "#10b981",
        },
      },
    },
    MuiTableHead: {
      styleOverrides: {
        root: {
          "& .MuiTableCell-head": {
            backgroundColor: mode === "light" ? "#f3f3f3" : "#2a2a2a",
            fontWeight: 600,
            color: mode === "light" ? "#666666" : "#a0a0a0",
            textTransform: "uppercase" as const,
            fontSize: "0.75rem",
            letterSpacing: "0.05em",
            borderBottom: `2px solid ${mode === "light" ? "#e0e0e0" : "#3a3a3a"}`,
          },
        },
      },
    },
    MuiTableCell: {
      styleOverrides: {
        // Unified row height across every list — one vertical padding for
        // default and small (size="small") cells so tables line up.
        root: {
          borderBottom: `1px solid ${mode === "light" ? "#e8e8e8" : "#3a3a3a"}`,
          padding: "8px 16px",
          // Fixed row height so every list lines up regardless of whether a
          // row carries a copy button / chip or just text. Cells that must
          // opt out (e.g. collapsible detail rows) set `height: "auto"`.
          height: 48,
        },
        sizeSmall: {
          padding: "8px 16px",
          height: 48,
        },
      },
    },
    MuiTableRow: {
      styleOverrides: {
        root: {
          "&:hover": {
            backgroundColor: mode === "light" ? "#f3f3f3" : "#3a3a3a",
          },
          "&:last-child td": {
            borderBottom: 0,
          },
        },
      },
    },
    MuiIconButton: {
      styleOverrides: {
        root: {
          transition: "all 0.2s ease-in-out",
          "&:hover": {
            backgroundColor: mode === "light" ? "#e8e8e8" : "#3a3a3a",
            transform: "scale(1.1)",
          },
        },
      },
    },
    MuiCircularProgress: {
      styleOverrides: {
        root: {
          color: "#ff6633",
        },
      },
    },
    MuiButton: {
      variants: [
        {
          props: { variant: "contained" as const, color: "primary" as const },
          style: {
            "&:hover": {
              backgroundColor: "#e55a2b",
            },
          },
        },
      ],
    },
  },
});

interface ThemeProviderProps {
  children: ReactNode;
}

export const ThemeProvider = ({ children }: ThemeProviderProps) => {
  const [mode, setMode] = useState<ThemeMode>(() => {
    const saved = localStorage.getItem("theme-mode");
    return (saved as ThemeMode) || "auto";
  });

  const [systemMode, setSystemMode] = useState<"light" | "dark">(() =>
    window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light"
  );

  useEffect(() => {
    const mediaQuery = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = (e: MediaQueryListEvent) => {
      setSystemMode(e.matches ? "dark" : "light");
    };
    mediaQuery.addEventListener("change", handler);
    return () => mediaQuery.removeEventListener("change", handler);
  }, []);

  useEffect(() => {
    localStorage.setItem("theme-mode", mode);
  }, [mode]);

  const resolvedMode = mode === "auto" ? systemMode : mode;

  useEffect(() => {
    if (resolvedMode === "dark") {
      document.documentElement.classList.add("dark");
    } else {
      document.documentElement.classList.remove("dark");
    }
  }, [resolvedMode]);

  const theme = useMemo(() => createTheme(getDesignTokens(resolvedMode)), [resolvedMode]);

  return (
    <ThemeContext.Provider value={{ mode, setMode, resolvedMode }}>
      <MuiThemeProvider theme={theme}>
        <CssBaseline />
        {children}
      </MuiThemeProvider>
    </ThemeContext.Provider>
  );
}
