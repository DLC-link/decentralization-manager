import { Box, Tooltip, Typography } from "@mui/material";
import { useTheme } from "@mui/material/styles";

// Distinct, mid-dark avatar fills — all clear ≥4.5:1 against white text.
const AVATAR_COLORS = [
  "#2A6FDB", // blue
  "#15803D", // green
  "#B45309", // amber
  "#7C3AED", // violet
  "#BE185D", // pink
  "#0F766E", // teal
  "#9A3412", // rust
  "#475569", // slate
];
const avatarColor = (id: string) => {
  let h = 0;
  for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) >>> 0;
  return AVATAR_COLORS[h % AVATAR_COLORS.length];
};

/**
 * M-of-N confirmation progress as a ring. The fill is clamped to 100% so an
 * over-quorum action never renders past full (no more "3 / 2 confirmed"), and
 * the ring turns success-green once quorum is met / the action can execute.
 */
export const ConfirmRing = ({
  count,
  threshold,
  canExecute,
}: {
  count: number;
  threshold: number;
  canExecute: boolean;
}) => {
  const theme = useTheme();
  const safe = Math.max(threshold, 1);
  const met = canExecute || count >= threshold;
  const frac = Math.min(count / safe, 1);
  const r = 13;
  const c = 2 * Math.PI * r;
  const stroke = met ? theme.palette.success.main : theme.palette.primary.main;
  return (
    <Box sx={{ display: "inline-flex", alignItems: "center", gap: 1 }}>
      <Box sx={{ position: "relative", width: 34, height: 34, flexShrink: 0 }}>
        <svg width="34" height="34" viewBox="0 0 34 34">
          <circle
            cx="17"
            cy="17"
            r={r}
            fill="none"
            strokeWidth="3"
            stroke={theme.palette.divider}
          />
          <circle
            cx="17"
            cy="17"
            r={r}
            fill="none"
            strokeWidth="3"
            strokeLinecap="round"
            stroke={stroke}
            strokeDasharray={`${frac * c} ${c}`}
            transform="rotate(-90 17 17)"
          />
        </svg>
        <Box
          sx={{
            position: "absolute",
            inset: 0,
            display: "grid",
            placeItems: "center",
            fontFamily: "var(--font-mono)",
            fontSize: 10,
            fontWeight: 600,
            color: "text.primary",
          }}
        >
          {met ? "✓" : `${count}/${threshold}`}
        </Box>
      </Box>
      <Typography
        sx={{
          fontFamily: "var(--font-mono)",
          fontSize: 12,
          color: "text.secondary",
          whiteSpace: "nowrap",
        }}
      >
        {met
          ? `${threshold} of ${threshold} confirmed · quorum met`
          : `${count} of ${threshold} confirmed`}
      </Typography>
    </Box>
  );
};

/** Stacked confirmed-by avatars (initials) + dashed pending slots up to threshold. */
export const ConfirmAvatars = ({
  confirmations,
  memberPartyId,
  threshold,
}: {
  confirmations: { confirming_party: string }[];
  memberPartyId?: string;
  threshold: number;
}) => {
  const confirmed = confirmations.slice(0, 4);
  const pending = Math.max(0, Math.min(4, threshold - confirmations.length));
  const initials = (p: string) =>
    (p.split("::")[0] || p)
      .replace(/[^a-zA-Z0-9]/g, "")
      .slice(0, 2)
      .toUpperCase();
  const dot = {
    width: 22,
    height: 22,
    borderRadius: "50%",
    border: "1.5px solid",
    borderColor: "background.paper",
    display: "grid",
    placeItems: "center",
    fontFamily: "var(--font-mono)",
    fontSize: 9,
    fontWeight: 600,
    flexShrink: 0,
  } as const;
  if (confirmed.length === 0 && pending === 0) return null;
  return (
    <Box sx={{ display: "flex", alignItems: "center" }}>
      {confirmed.map((c, i) => {
        const own = c.confirming_party === memberPartyId;
        return (
          <Tooltip
            key={c.confirming_party + i}
            title={own ? `${c.confirming_party} (you)` : c.confirming_party}
          >
            <Box
              sx={{
                ...dot,
                ml: i === 0 ? 0 : "-7px",
                bgcolor: own
                  ? "primary.main"
                  : avatarColor(c.confirming_party),
                color: "#fff",
              }}
            >
              {own ? "ME" : initials(c.confirming_party)}
            </Box>
          </Tooltip>
        );
      })}
      {Array.from({ length: pending }).map((_, i) => (
        <Tooltip key={`p${i}`} title="Awaiting confirmation">
          <Box
            sx={{
              ...dot,
              ml: confirmed.length === 0 && i === 0 ? 0 : "-7px",
              border: "1.5px dashed",
              borderColor: "divider",
              bgcolor: "transparent",
            }}
          />
        </Tooltip>
      ))}
    </Box>
  );
};
