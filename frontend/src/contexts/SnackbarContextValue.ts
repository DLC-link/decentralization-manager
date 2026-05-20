import { createContext, useContext } from "react";

export type SnackbarSeverity = "info" | "error";

export interface SnackbarContextType {
  /// Errors (`severity: "error"`) stay open until dismissed and render with
  /// red styling + a close button. Everything else uses the lightweight
  /// auto-hiding toast.
  showSnackbar: (message: string, severity?: SnackbarSeverity) => void;
}

export const SnackbarContext = createContext<SnackbarContextType | undefined>(
  undefined,
);

export const useSnackbar = () => {
  const context = useContext(SnackbarContext);
  if (!context) {
    throw new Error("useSnackbar must be used within SnackbarProvider");
  }
  return context;
};
