import { Fragment, useState, type ReactNode } from "react";
import { Box, Button, Typography } from "@mui/material";
import { alpha, keyframes, useTheme } from "@mui/material/styles";
import type { WorkflowProgress } from "../../types";

type PillTone = "accent" | "neutral" | "success" | "danger";

const pulse = keyframes({ "50%": { opacity: 0.4 } });
const spin = keyframes({ to: { transform: "rotate(360deg)" } });

const REDUCED = "@media (prefers-reduced-motion: reduce)";

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
        {met ? "quorum met" : `${count} of ${threshold}`}
      </Typography>
    </Box>
  );
};

/** Generic BitSafe status/state pill — a pulsing dot for live items. */
export const Pill = ({
  label,
  tone = "neutral",
  live = false,
}: {
  label: string;
  tone?: PillTone;
  live?: boolean;
}) => {
  const theme = useTheme();
  const color =
    tone === "accent"
      ? theme.palette.primary.main
      : tone === "success"
        ? theme.palette.success.main
        : tone === "danger"
          ? theme.palette.error.main
          : theme.palette.text.secondary;
  return (
    <Box
      sx={{
        display: "inline-flex",
        alignItems: "center",
        gap: 0.75,
        px: 1,
        py: 0.4,
        borderRadius: "6px",
        bgcolor: alpha(color, tone === "neutral" ? 0.12 : 0.16),
        color,
        fontFamily: "var(--font-sans)",
        fontSize: 11,
        fontWeight: tone === "accent" ? 700 : 600,
        whiteSpace: "nowrap",
      }}
    >
      {live && (
        <Box
          sx={{
            width: 6,
            height: 6,
            borderRadius: "50%",
            bgcolor: color,
            animation: `${pulse} 1.4s ease-in-out infinite`,
            [REDUCED]: { animation: "none" },
          }}
        />
      )}
      {label}
    </Box>
  );
};

/** Workflow status → pill. */
export const StatusPill = ({ status }: { status: WorkflowProgress }) => {
  const map: Record<
    WorkflowProgress,
    { label: string; tone: PillTone; live: boolean }
  > = {
    inprogress: { label: "Running", tone: "accent", live: true },
    idle: { label: "Queued", tone: "neutral", live: false },
    completed: { label: "Completed", tone: "success", live: false },
    failed: { label: "Failed", tone: "danger", live: false },
    cancelled: { label: "Cancelled", tone: "neutral", live: false },
  };
  const cfg = map[status] ?? { label: status, tone: "neutral", live: false };
  return <Pill label={cfg.label} tone={cfg.tone} live={cfg.live} />;
};

/**
 * Shared card chrome for every approvals item: glyph + type eyebrow, a status
 * pill and relative time, a human title, key facts, a footer (ring/info on the
 * left, actions on the right) and an optional inline "Review" expander.
 */
export const ApprovalCard = ({
  glyph,
  eyebrow,
  accent = false,
  pill,
  time,
  title,
  facts,
  footerLeft,
  actions,
  detail,
}: {
  glyph: string;
  eyebrow: string;
  accent?: boolean;
  pill?: ReactNode;
  time?: ReactNode;
  title?: ReactNode;
  facts?: ReactNode;
  footerLeft?: ReactNode;
  actions?: ReactNode;
  detail?: ReactNode;
}) => {
  const [open, setOpen] = useState(false);
  return (
    <Box
      sx={{
        position: "relative",
        p: "18px 20px",
        borderRadius: "12px",
        border: "1px solid",
        borderColor: "divider",
        bgcolor: "background.paper",
        ...(accent && {
          boxShadow: (t) => `inset 3px 0 0 ${t.palette.primary.main}`,
        }),
      }}
    >
      <Box
        sx={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 1.5,
        }}
      >
        <Box
          sx={{
            display: "flex",
            alignItems: "center",
            gap: 1,
            minWidth: 0,
            color: "text.secondary",
            fontFamily: "var(--font-mono)",
            fontSize: 11,
            fontWeight: 500,
            letterSpacing: "0.06em",
            textTransform: "uppercase",
          }}
        >
          <Box component="span" sx={{ color: "text.disabled", fontSize: 13, flexShrink: 0 }}>
            {glyph}
          </Box>
          <Box
            component="span"
            sx={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}
          >
            {eyebrow}
          </Box>
        </Box>
        <Box sx={{ display: "flex", alignItems: "center", gap: 1.25, flexShrink: 0 }}>
          {pill}
          {time != null && (
            <Box
              component="span"
              sx={{
                fontFamily: "var(--font-mono)",
                fontSize: 12,
                color: "text.secondary",
                whiteSpace: "nowrap",
              }}
            >
              {time}
            </Box>
          )}
        </Box>
      </Box>

      {title != null && (
        <Box
          sx={{
            mt: 1.25,
            fontFamily: "var(--font-sans)",
            fontSize: 16,
            fontWeight: 500,
            lineHeight: 1.35,
          }}
        >
          {title}
        </Box>
      )}
      {facts != null && <Box sx={{ mt: 1 }}>{facts}</Box>}

      {(footerLeft != null || actions != null || detail != null) && (
        <Box
          sx={{
            mt: 2,
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            gap: 1.5,
            flexWrap: "wrap",
          }}
        >
          <Box sx={{ minWidth: 0 }}>{footerLeft}</Box>
          <Box sx={{ display: "flex", gap: 1, alignItems: "center", flexShrink: 0 }}>
            {detail != null && (
              <Button
                size="small"
                onClick={() => setOpen((o) => !o)}
                sx={{ color: "text.secondary", minWidth: 0 }}
              >
                Review {open ? "▴" : "▾"}
              </Button>
            )}
            {actions}
          </Box>
        </Box>
      )}

      {detail != null && open && (
        <Box sx={{ mt: 1.75, pt: 1.75, borderTop: "1px solid", borderColor: "divider" }}>
          {detail}
        </Box>
      )}
    </Box>
  );
};

/**
 * Live step pipeline for a running workflow. Only `step_index`/`step_total`
 * and the current step name are known, so completed steps render as ✓, the
 * current as a spinning marker (with its name below), and the rest as numbers.
 */
export const WorkflowPipeline = ({
  current,
  total,
  label,
}: {
  current: number;
  total: number;
  label: string;
}) => {
  const theme = useTheme();
  if (total <= 0) return null;
  const steps = Array.from({ length: total }, (_, i) => i);
  return (
    <Box sx={{ display: "flex", flexDirection: "column", gap: 0.75 }}>
      <Box sx={{ display: "flex", alignItems: "center", overflowX: "auto", pb: 0.5 }}>
        {steps.map((i) => {
          const done = i < current;
          const active = i === current;
          return (
            <Fragment key={i}>
              {i > 0 && (
                <Box
                  sx={{
                    flex: 1,
                    minWidth: 12,
                    height: "1.5px",
                    mx: 0.5,
                    bgcolor: i <= current ? "success.main" : "divider",
                  }}
                />
              )}
              <Box
                sx={{
                  flexShrink: 0,
                  width: 22,
                  height: 22,
                  borderRadius: "50%",
                  display: "grid",
                  placeItems: "center",
                  fontFamily: "var(--font-mono)",
                  fontSize: 11,
                  fontWeight: 700,
                  ...(done && {
                    bgcolor: alpha(theme.palette.success.main, 0.16),
                    color: "success.main",
                    border: `1.5px solid ${theme.palette.success.main}`,
                  }),
                  ...(active && { bgcolor: "primary.main", color: "#fff" }),
                  ...(!done &&
                    !active && {
                      color: "text.disabled",
                      border: "1.5px solid",
                      borderColor: "divider",
                    }),
                }}
              >
                {done ? (
                  "✓"
                ) : active ? (
                  <Box
                    component="span"
                    sx={{
                      display: "inline-block",
                      animation: `${spin} 1.4s linear infinite`,
                      [REDUCED]: { animation: "none" },
                    }}
                  >
                    ⟳
                  </Box>
                ) : (
                  i + 1
                )}
              </Box>
            </Fragment>
          );
        })}
      </Box>
      <Box
        sx={{
          display: "flex",
          justifyContent: "space-between",
          alignItems: "baseline",
          gap: 1,
        }}
      >
        <Typography variant="body2" sx={{ fontWeight: 600 }}>
          {label}
        </Typography>
        <Typography
          variant="caption"
          color="text.secondary"
          sx={{ fontFamily: "var(--font-mono)" }}
        >
          {current + 1} / {total}
        </Typography>
      </Box>
    </Box>
  );
};
