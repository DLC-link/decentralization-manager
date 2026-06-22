import { useState, useEffect, useMemo, type ReactNode } from "react";
import { ThemeProvider as MuiThemeProvider, createTheme } from "@mui/material/styles";
import { CssBaseline } from "@mui/material";
import { ThemeContext, type ThemeMode } from "./ThemeContextValue";

// BitSafe Design System — faithful implementation of the `bitsafe-design`
// skill tokens (Figma /01-Foundations). Dark is the default: a warm-neutral
// near-black substrate with the #D63A0F action accent. Space Grotesk for text,
// Roboto Mono for every number / address / machine string.
const FONT_SANS =
  '"Space Grotesk", system-ui, -apple-system, "Segoe UI", sans-serif';
const FONT_MONO = "var(--font-mono)";

// Brand action accent (orange-700 ramp). #FF6633 is decorative-only and never
// used as a fill behind white text.
const ACCENT = "#D63A0F";
const ACCENT_HOVER = "#C03D10";
const ACCENT_PRESS = "#A82E08";
const ACCENT_LIGHT = "#E84E1B";
const ACCENT_TINT = "rgba(214,58,15,0.08)";

const tokens = {
  dark: {
    bg: "#0F0E0D", // stone-950 page
    surface: "#1E1A17", // stone-850 card fill
    raised: "#1A1714", // stone-900
    surface2: "#241F1C",
    border: "#2A2420", // stone-800
    text: "#FFFFFF",
    text2: "#A89B92", // warm secondary (lightened stone-400/500 for legibility)
    chipBg: "#2A2420",
    chipText: "#C9C2BE",
    rowHover: "rgba(255,255,255,0.03)",
    rowAlt: "rgba(255,255,255,0.02)",
    tooltipBg: "#2A2420",
    success: "#34D399",
    error: "#F2635A",
    warning: "#EAB308",
    info: "#418DF0",
  },
  light: {
    bg: "#FAF9F8", // stone-75 warm off-white
    surface: "#FFFFFF",
    raised: "#FFFFFF",
    surface2: "#F2F0EE",
    border: "#E0DBDA", // stone-200
    text: "#1A1714", // stone-900
    text2: "#66605C", // stone-600
    chipBg: "#F2F0EE",
    chipText: "#525252",
    rowHover: "rgba(0,0,0,0.02)",
    rowAlt: "rgba(0,0,0,0.015)",
    tooltipBg: "#1A1714",
    success: "#0E7C5A",
    error: "#C0341B",
    warning: "#B58900",
    info: "#2A6FDB",
  },
} as const;

const getDesignTokens = (mode: "light" | "dark") => {
  const t = tokens[mode];

  return {
    palette: {
      mode,
      primary: {
        main: ACCENT,
        light: ACCENT_LIGHT,
        dark: ACCENT_PRESS,
        contrastText: "#FFFFFF",
      },
      secondary:
        mode === "dark"
          ? { main: "#FFFFFF", light: "#FFFFFF", dark: "#C9C2BE", contrastText: "#1A1714" }
          : { main: "#1A1714", light: "#3A332E", dark: "#000000", contrastText: "#FFFFFF" },
      success: { main: t.success, contrastText: "#FFFFFF" },
      error: { main: t.error, contrastText: "#FFFFFF" },
      warning: { main: t.warning, contrastText: "#1A1714" },
      info: { main: t.info, contrastText: "#FFFFFF" },
      background: { default: t.bg, paper: t.surface },
      text: { primary: t.text, secondary: t.text2 },
      divider: t.border,
    },
    typography: {
      fontFamily: FONT_SANS,
      h4: { fontWeight: 700, letterSpacing: "-0.5px" },
      h5: { fontWeight: 500, letterSpacing: "-0.5px" },
      h6: { fontWeight: 500, letterSpacing: "-0.25px" },
      subtitle1: { fontWeight: 600 },
      // Eyebrow / small-cap label — tracked-out uppercase Roboto Mono.
      subtitle2: {
        fontFamily: FONT_MONO,
        fontWeight: 500,
        color: t.text2,
        textTransform: "uppercase" as const,
        fontSize: "0.7rem",
        letterSpacing: "0.12em",
      },
      overline: {
        fontFamily: FONT_MONO,
        fontWeight: 500,
        letterSpacing: "0.12em",
      },
      button: { textTransform: "none" as const, fontWeight: 600 },
    },
    shape: { borderRadius: 12 },
    components: {
      MuiCssBaseline: {
        styleOverrides: {
          body: { backgroundColor: t.bg },
          "::selection": { background: ACCENT_TINT },
        },
      },
      MuiDialog: {
        defaultProps: { disableScrollLock: true },
        styleOverrides: {
          paper: {
            backgroundImage: "none",
            border: `1px solid ${t.border}`,
            boxShadow: "0 8px 32px rgba(0,0,0,0.40)",
          },
        },
      },
      MuiMenu: {
        styleOverrides: {
          paper: {
            backgroundImage: "none",
            border: `1px solid ${t.border}`,
            boxShadow: "0 4px 16px rgba(0,0,0,0.30)",
          },
        },
      },
      MuiPopover: {
        styleOverrides: {
          paper: { backgroundImage: "none", border: `1px solid ${t.border}` },
        },
      },
      MuiCard: {
        defaultProps: { elevation: 0 },
        styleOverrides: {
          root: {
            // Card anatomy: surface fill + 1px border + 12px radius, no rest shadow.
            backgroundImage: "none",
            boxShadow: "none",
            border: `1px solid ${t.border}`,
            borderRadius: 12,
          },
        },
      },
      MuiAccordion: {
        defaultProps: { elevation: 0 },
        styleOverrides: {
          root: {
            backgroundImage: "none",
            boxShadow: "none",
            border: `1px solid ${t.border}`,
            borderRadius: 12,
            "&:before": { display: "none" },
            "&.Mui-expanded": { marginTop: 0, marginBottom: 16 },
          },
        },
      },
      MuiAccordionSummary: {
        styleOverrides: {
          root: {
            backgroundColor: t.raised,
            borderRadius: "12px 12px 0 0",
            flexDirection: "row-reverse" as const,
            gap: 8,
            "&.Mui-expanded": { minHeight: 48 },
          },
          content: { "&.Mui-expanded": { margin: "12px 0" } },
          expandIconWrapper: { marginRight: 0, marginLeft: 0 },
        },
      },
      MuiButton: {
        defaultProps: { disableElevation: true },
        styleOverrides: {
          root: { borderRadius: 6, fontWeight: 600 },
        },
        variants: [
          {
            props: { variant: "contained" as const, color: "primary" as const },
            style: {
              backgroundColor: ACCENT,
              "&:hover": { backgroundColor: ACCENT_HOVER },
              "&:active": { backgroundColor: ACCENT_PRESS },
            },
          },
          {
            props: { variant: "outlined" as const, color: "primary" as const },
            style: {
              borderColor: ACCENT,
              "&:hover": { borderColor: ACCENT_HOVER, backgroundColor: ACCENT_TINT },
            },
          },
        ],
      },
      MuiChip: {
        styleOverrides: {
          root: { fontWeight: 500, borderRadius: 4 },
          colorDefault: { backgroundColor: t.chipBg, color: t.chipText },
          colorPrimary: { backgroundColor: ACCENT, color: "#FFFFFF" },
          colorSuccess: { backgroundColor: t.success, color: "#FFFFFF" },
        },
      },
      MuiTableHead: {
        styleOverrides: {
          root: {
            "& .MuiTableCell-head": {
              backgroundColor: t.raised,
              fontFamily: FONT_MONO,
              fontWeight: 500,
              color: t.text2,
              textTransform: "uppercase" as const,
              fontSize: "0.7rem",
              letterSpacing: "0.12em",
              borderBottom: `1px solid ${t.border}`,
            },
          },
        },
      },
      MuiTableCell: {
        styleOverrides: {
          // Unified row height across every list — one vertical padding for
          // default and small (size="small") cells so tables line up.
          root: {
            borderBottom: `1px solid ${t.border}`,
            padding: "8px 16px",
            // Fixed row height so every list lines up regardless of whether a
            // row carries a copy button / chip or just text. Cells that must
            // opt out (e.g. collapsible detail rows) set `height: "auto"`.
            height: 48,
            // On large monitors inset the leading/trailing cell content so the
            // table content pulls together toward a ~1300px column, while the
            // row rules and zebra fills still run full-bleed (edge to edge).
            "&:first-of-type": {
              paddingLeft: "max(16px, calc((100vw - 1560px) / 2))",
            },
            "&:last-of-type": {
              paddingRight: "max(16px, calc((100vw - 1560px) / 2))",
            },
          },
          sizeSmall: { padding: "8px 16px", height: 48 },
        },
      },
      MuiTableRow: {
        styleOverrides: {
          root: {
            "&:hover": { backgroundColor: t.rowHover },
            "&:last-child td": { borderBottom: 0 },
          },
        },
      },
      MuiTabs: {
        styleOverrides: { indicator: { backgroundColor: ACCENT } },
      },
      MuiTab: {
        styleOverrides: { root: { textTransform: "none" as const, fontWeight: 600 } },
      },
      MuiOutlinedInput: {
        styleOverrides: {
          root: {
            borderRadius: 6,
            "&.Mui-focused .MuiOutlinedInput-notchedOutline": {
              borderColor: ACCENT,
              borderWidth: 1.5,
            },
          },
        },
      },
      MuiIconButton: {
        styleOverrides: {
          root: {
            // Linear hover tint — no scale transform.
            transition: "background-color 150ms cubic-bezier(0.2,0,0,1), color 150ms cubic-bezier(0.2,0,0,1)",
            "&:hover": { backgroundColor: ACCENT_TINT },
          },
        },
      },
      MuiCircularProgress: { styleOverrides: { root: { color: ACCENT } } },
      MuiLinearProgress: {
        styleOverrides: {
          root: { backgroundColor: t.border },
          bar: { backgroundColor: ACCENT },
        },
      },
      MuiFab: {
        styleOverrides: {
          primary: { backgroundColor: ACCENT, "&:hover": { backgroundColor: ACCENT_HOVER } },
        },
      },
      MuiTooltip: {
        styleOverrides: {
          tooltip: {
            backgroundColor: t.tooltipBg,
            border: `1px solid ${t.border}`,
            color: mode === "dark" ? "#FFFFFF" : "#FFFFFF",
            fontSize: "0.72rem",
          },
          arrow: { color: t.tooltipBg },
        },
      },
      MuiLink: {
        styleOverrides: { root: { color: t.info, textDecorationColor: t.info } },
      },
    },
  };
};

interface ThemeProviderProps {
  children: ReactNode;
}

export const ThemeProvider = ({ children }: ThemeProviderProps) => {
  const [mode, setMode] = useState<ThemeMode>(() => {
    const saved = localStorage.getItem("theme-mode");
    // BitSafe is dark-first: default new visitors to dark rather than the OS.
    return (saved as ThemeMode) || "dark";
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
