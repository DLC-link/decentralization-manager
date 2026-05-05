import { createContext, useContext } from "react";

export type ThemeMode = "light" | "dark" | "auto";

export interface ThemeContextType {
  mode: ThemeMode;
  setMode: (mode: ThemeMode) => void;
  resolvedMode: "light" | "dark";
}

export const ThemeContext = createContext<ThemeContextType | undefined>(
  undefined,
);

export const useThemeMode = () => {
  const context = useContext(ThemeContext);
  if (!context) {
    throw new Error("useThemeMode must be used within ThemeProvider");
  }
  return context;
};
