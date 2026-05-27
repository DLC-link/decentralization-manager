import { useState, useCallback, type ReactNode } from "react";
import { Alert, Snackbar } from "@mui/material";
import { SnackbarContext, type SnackbarSeverity } from "./SnackbarContextValue";

interface SnackbarProviderProps {
  children: ReactNode;
}

export const SnackbarProvider = ({ children }: SnackbarProviderProps) => {
  const [open, setOpen] = useState(false);
  const [message, setMessage] = useState("");
  const [severity, setSeverity] = useState<SnackbarSeverity>("info");

  const showSnackbar = useCallback(
    (msg: string, sev: SnackbarSeverity = "info") => {
      setMessage(msg);
      setSeverity(sev);
      setOpen(true);
    },
    [],
  );

  const handleClose = (_event?: unknown, reason?: string) => {
    // Errors stay open until the user explicitly dismisses them — ignore the
    // clickaway/auto-hide channels so the message can be read.
    if (severity === "error" && reason !== "explicit") return;
    setOpen(false);
  };

  const isError = severity === "error";

  return (
    <SnackbarContext.Provider value={{ showSnackbar }}>
      {children}
      <Snackbar
        open={open}
        autoHideDuration={isError ? null : 2000}
        onClose={handleClose}
        anchorOrigin={{ vertical: "bottom", horizontal: "left" }}
        sx={{ zIndex: 9999 }}
      >
        <Alert
          severity={isError ? "error" : "info"}
          variant={isError ? "filled" : "standard"}
          onClose={() => handleClose(undefined, "explicit")}
          sx={{ width: "100%" }}
        >
          {message}
        </Alert>
      </Snackbar>
    </SnackbarContext.Provider>
  );
};
