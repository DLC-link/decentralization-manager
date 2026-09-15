import { Box, Tooltip } from "@mui/material";

/**
 * How a dot reads, independent of what produced it. Node-health probes and
 * peer heartbeats report different vocabularies but share one indicator, so
 * each wire enum maps onto this tone instead of driving the colors directly.
 * `toneForPeer` in `../peers` maps a peer's status.
 */
export type DotTone = "self" | "live" | "ok" | "warn" | "bad" | "idle";

interface StatusDotProps {
  tone?: DotTone;
  /** Tooltip text; when set the dot shows a help cursor. */
  title?: string;
  /** Dot diameter in px. */
  size?: number;
}

// Only a probe that just answered emits the soft pulsing halo — the one
// continuous motion blessed by the BitSafe design system. Every other state is
// static and colored by tone. Honors prefers-reduced-motion.
const TONE: Record<DotTone, { color: string; pulse: boolean }> = {
  self: { color: "primary.main", pulse: false },
  live: { color: "success.main", pulse: true },
  ok: { color: "success.main", pulse: false },
  warn: { color: "warning.main", pulse: false },
  bad: { color: "error.main", pulse: false },
  idle: { color: "text.disabled", pulse: false },
};

export const StatusDot = ({ tone, title, size = 9 }: StatusDotProps) => {
  const cfg = TONE[tone ?? "idle"];

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
