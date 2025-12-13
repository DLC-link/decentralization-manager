import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { ThemeProvider, SnackbarProvider } from "./contexts";
import "./index.css";
import App from "./App.tsx";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ThemeProvider>
      <SnackbarProvider>
        <App />
      </SnackbarProvider>
    </ThemeProvider>
  </StrictMode>,
);
