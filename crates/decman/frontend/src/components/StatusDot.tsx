import { Box, Tooltip } from "@mui/material";
import type { ConnectionStatus } from "../types";

interface StatusDotProps {
  status?: ConnectionStatus;
  /** Tooltip text; when set the dot shows a help cursor. */
  title?: string;
  /** Dot diameter in px. */
  size?: number;
}

// Peer connection indicator. Only a live (Connected) peer emits the soft
// pulsing halo — the one continuous motion blessed by the BitSafe design
// system. Offline / failed / local states are static, colored by state:
// connected (green), unreachable (red), handshake-failed (amber), you (accent).
// Honors prefers-reduced-motion.
const STATUS: Record<ConnectionStatus, { color: string; pulse: boolean }> = {
  Connected: { color: "success.main", pulse: true },
  CurrentNode: { color: "primary.main", pulse: false },
  Unreachable: { color: "error.main", pulse: false },
  HandshakeFailed: { color: "warning.main", pulse: false },
};

export const StatusDot = ({ status, title, size = 9 }: StatusDotProps) => {
  const cfg = (status && STATUS[status]) || { color: "text.disabled", pulse: false };

  const dot = (
    <Box
      sx={{
        position: "relative",
        display: "inline-flex",
        width: size,
        height: size,
        cursor: title ? "help" : "default",
        verticalAlign: "middle",
      }}
    >
      {cfg.pulse && (
        <Box
          sx={{
            position: "absolute",
            inset: 0,
            borderRadius: "50%",
            bgcolor: cfg.color,
            animation: "bsPing 1.4s cubic-bezier(0.2,0,0,1) infinite",
            "@keyframes bsPing": {
              "0%": { transform: "scale(1)", opacity: 0.55 },
              "70%, 100%": { transform: "scale(2.6)", opacity: 0 },
            },
            "@media (prefers-reduced-motion: reduce)": { display: "none" },
          }}
        />
      )}
      <Box
        sx={{
          position: "relative",
          width: size,
          height: size,
          borderRadius: "50%",
          bgcolor: cfg.color,
        }}
      />
    </Box>
  );

  return title ? (
    <Tooltip title={title} arrow>
      {dot}
    </Tooltip>
  ) : (
    dot
  );
};
