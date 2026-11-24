import { useState, type ReactNode } from "react";
import { Box, Button } from "@mui/material";
import { alpha, useTheme } from "@mui/material/styles";
import type { WorkflowProgress } from "../../types";
import { pulse, REDUCED } from "./motion";

type PillTone = "accent" | "neutral" | "success" | "danger";

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
  accent = false,
  pill,
  time,
  title,
  facts,
  footerLeft,
  actions,
  detail,
  dataAttrs,
}: {
  accent?: boolean;
  pill?: ReactNode;
  time?: ReactNode;
  title?: ReactNode;
  facts?: ReactNode;
  footerLeft?: ReactNode;
  actions?: ReactNode;
  detail?: ReactNode;
  /** `data-*` hooks set on the card root, for the e2e suite to select by. */
  dataAttrs?: Record<string, string>;
}) => {
  const [open, setOpen] = useState(false);
  return (
    <Box
      {...dataAttrs}
      sx={{
        position: "relative",
        p: "14px 20px",
        borderRadius: "12px",
        border: "1px solid",
        borderColor: "divider",
        bgcolor: "background.paper",
        transition: "border-color 0.15s ease-out",
        "&:hover": {
          borderColor: (t) => (t.palette.mode === "dark" ? "#3A332E" : "#C9C2BE"),
        },
        ...(accent && {
          boxShadow: (t) => `inset 3px 0 0 ${t.palette.primary.main}`,
        }),
      }}
    >
      <Box
        sx={{
          display: "flex",
          alignItems: "flex-start",
          justifyContent: "space-between",
          gap: 1.5,
        }}
      >
        {title != null && (
          <Box
            sx={{
              minWidth: 0,
              fontFamily: "var(--font-sans)",
              fontSize: 16,
              fontWeight: 500,
              lineHeight: 1.35,
            }}
          >
            {title}
          </Box>
        )}
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

      {facts != null && <Box sx={{ mt: 1.25 }}>{facts}</Box>}

      {(footerLeft != null || actions != null || detail != null) && (
        <Box
          sx={{
            mt: 1.25,
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
