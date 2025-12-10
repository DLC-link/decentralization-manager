import { createContext, useContext, useState, useEffect, useMemo, type ReactNode } from "react";
import { ThemeProvider as MuiThemeProvider, createTheme } from "@mui/material/styles";
import { CssBaseline } from "@mui/material";

type ThemeMode = "light" | "dark" | "auto";

interface ThemeContextType {
  mode: ThemeMode;
  setMode: (mode: ThemeMode) => void;
  resolvedMode: "light" | "dark";
}

const ThemeContext = createContext<ThemeContextType | undefined>(undefined);

export const useThemeMode = () => {
  const context = useContext(ThemeContext);
  if (!context) {
    throw new Error("useThemeMode must be used within ThemeProvider");
  }
  return context;
};

const getDesignTokens = (mode: "light" | "dark") => ({
  palette: {
    mode,
    primary: {
      main: "#6366f1",
      light: "#818cf8",
      dark: "#4f46e5",
    },
    secondary: {
      main: "#ec4899",
      light: "#f472b6",
      dark: "#db2777",
    },
    success: {
      main: "#10b981",
      light: "#34d399",
      dark: "#059669",
    },
    ...(mode === "light"
      ? {
          background: {
            default: "#f8fafc",
            paper: "#ffffff",
          },
          text: {
            primary: "#1e293b",
            secondary: "#64748b",
          },
        }
      : {
          background: {
            default: "#0f172a",
            paper: "#1e293b",
          },
          text: {
            primary: "#f1f5f9",
            secondary: "#94a3b8",
          },
        }),
  },
  typography: {
    fontFamily: '"Inter", "SF Pro Display", -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif',
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
      color: mode === "light" ? "#64748b" : "#94a3b8",
      textTransform: "uppercase" as const,
      fontSize: "0.75rem",
      letterSpacing: "0.05em",
    },
  },
  shape: {
    borderRadius: 12,
  },
  components: {
    MuiCard: {
      styleOverrides: {
        root: {
          boxShadow: mode === "light"
            ? "0 1px 3px 0 rgb(0 0 0 / 0.1), 0 1px 2px -1px rgb(0 0 0 / 0.1)"
            : "0 1px 3px 0 rgb(0 0 0 / 0.3), 0 1px 2px -1px rgb(0 0 0 / 0.3)",
          border: `1px solid ${mode === "light" ? "#e2e8f0" : "#334155"}`,
        },
      },
    },
    MuiAccordion: {
      styleOverrides: {
        root: {
          boxShadow: mode === "light"
            ? "0 1px 3px 0 rgb(0 0 0 / 0.1), 0 1px 2px -1px rgb(0 0 0 / 0.1)"
            : "0 1px 3px 0 rgb(0 0 0 / 0.3), 0 1px 2px -1px rgb(0 0 0 / 0.3)",
          border: `1px solid ${mode === "light" ? "#e2e8f0" : "#334155"}`,
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
          backgroundColor: mode === "light" ? "#f8fafc" : "#1e293b",
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
          backgroundColor: mode === "light" ? "#f1f5f9" : "#334155",
          color: mode === "light" ? "#475569" : "#cbd5e1",
        },
        colorPrimary: {
          backgroundColor: "#6366f1",
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
            backgroundColor: mode === "light" ? "#f8fafc" : "#1e293b",
            fontWeight: 600,
            color: mode === "light" ? "#64748b" : "#94a3b8",
            textTransform: "uppercase" as const,
            fontSize: "0.75rem",
            letterSpacing: "0.05em",
            borderBottom: `2px solid ${mode === "light" ? "#e2e8f0" : "#334155"}`,
          },
        },
      },
    },
    MuiTableCell: {
      styleOverrides: {
        root: {
          borderBottom: `1px solid ${mode === "light" ? "#f1f5f9" : "#334155"}`,
          padding: "12px 16px",
        },
      },
    },
    MuiTableRow: {
      styleOverrides: {
        root: {
          "&:hover": {
            backgroundColor: mode === "light" ? "#f8fafc" : "#334155",
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
            backgroundColor: mode === "light" ? "#f1f5f9" : "#334155",
            transform: "scale(1.1)",
          },
        },
      },
    },
    MuiCircularProgress: {
      styleOverrides: {
        root: {
          color: "#6366f1",
        },
      },
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
