import { Fragment, useEffect, useState } from "react";
import { Box, LinearProgress, Tooltip, Typography } from "@mui/material";
import { alpha, useTheme } from "@mui/material/styles";
import type { AcsTransferProgress } from "../../types";
import type { WorkflowStepInfo } from "../../workflowSteps";
import { pulse, REDUCED } from "./motion";

/** How often the meter re-reads the clock, so "no movement for Nm" advances. */
const ACS_TICK_MS = 5_000;

/**
 * The current time, re-read on an interval.
 *
 * Reading `Date.now()` during render is impure and would only update when the
 * component happened to re-render, which is exactly when a stalled transfer
 * stops causing renders.
 */
const useTicking = (intervalMs: number): number => {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(id);
  }, [intervalMs]);
  return now;
};

/** A transfer with no sample this recent is treated as stalled, not moving. */
const ACS_STALL_AFTER_MS = 45_000;

const BYTE_UNITS = ["B", "KiB", "MiB", "GiB", "TiB"];

const formatBytes = (n: number): string => {
  let value = Math.max(0, n);
  let unit = 0;
  while (value >= 1024 && unit < BYTE_UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const shown = unit === 0 ? String(Math.round(value)) : value.toFixed(value < 10 ? 2 : 1);
  return `${shown} ${BYTE_UNITS[unit]}`;
};

const formatElapsed = (ms: number): string => {
  const total = Math.max(0, Math.round(ms / 1000));
  if (total < 60) return `${total}s`;
  const minutes = Math.floor(total / 60);
  if (minutes < 60) return `${minutes}m ${total % 60}s`;
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
};

/**
 * Movement on an ACS transfer.
 *
 * Indeterminate on purpose. Canton streams the snapshot as bare chunks with no
 * length and no contract count, and counting the party's contracts up front
 * would materialize the whole ACS, which is the read that has OOM'd nodes. So
 * this shows how much has moved and how fast, never a percentage: for a
 * transfer that can run for hours, "moving at 4.5 MB/s" is the useful signal,
 * and a fake percentage would be worse than none.
 */
export const AcsTransferMeter = ({
  progress,
}: {
  progress: AcsTransferProgress;
}) => {
  const theme = useTheme();
  const now = useTicking(ACS_TICK_MS);
  const elapsedMs = Math.max(0, progress.updated_at_ms - progress.started_at_ms);
  const bytesPerSecond = elapsedMs > 0 ? (progress.bytes * 1000) / elapsedMs : 0;
  const sinceSampleMs = Math.max(0, now - progress.updated_at_ms);
  const stalled = sinceSampleMs > ACS_STALL_AFTER_MS;
  const label = progress.direction === "export" ? "Sending ACS" : "Importing ACS";

  const facts = [
    formatBytes(progress.bytes),
    bytesPerSecond > 0 ? `${formatBytes(bytesPerSecond)}/s` : null,
    elapsedMs > 0 ? formatElapsed(elapsedMs) : null,
    progress.block > 0 ? `block ${progress.block}` : null,
  ].filter(Boolean);

  return (
    <Box>
      <Box
        sx={{
          display: "flex",
          alignItems: "baseline",
          justifyContent: "space-between",
          gap: 1,
          mb: 0.5,
        }}
      >
        <Typography sx={{ fontSize: 12, fontWeight: 600 }}>{label}</Typography>
        <Typography
          sx={{
            fontSize: 12,
            color: stalled ? "warning.main" : "text.secondary",
            fontVariantNumeric: "tabular-nums",
          }}
        >
          {stalled
            ? `no movement for ${formatElapsed(sinceSampleMs)}`
            : facts.join(" · ")}
        </Typography>
      </Box>
      <Tooltip
        title={
          stalled
            ? "The last progress sample is old. The transfer may be retrying a block, or the node may be down."
            : "Canton streams the snapshot without a total, so there is no percentage to show."
        }
      >
        <LinearProgress
          variant={stalled ? "determinate" : "indeterminate"}
          value={stalled ? 100 : undefined}
          color={stalled ? "warning" : "primary"}
          sx={{
            height: 4,
            borderRadius: 2,
            bgcolor: alpha(theme.palette.text.primary, 0.08),
            [REDUCED]: { "& .MuiLinearProgress-bar": { animation: "none" } },
          }}
        />
      </Tooltip>
    </Box>
  );
};

/**
 * Live step pipeline for a running workflow: completed steps render as ✓, the
 * current as a green pulsing dot, the rest as empty circles. `steps` labels the
 * dots on hover; without it they stay bare, which is what a run whose step list
 * the frontend cannot vouch for falls back to.
 */
export const WorkflowPipeline = ({
  current,
  total,
  steps,
}: {
  current: number;
  total: number;
  steps?: WorkflowStepInfo[] | null;
}) => {
  const theme = useTheme();
  if (total <= 0) return null;
  const dots = Array.from({ length: total }, (_, i) => i);
  return (
    <Box
      sx={{
        display: "flex",
        alignItems: "center",
        overflowX: "auto",
        // `overflow-x: auto` also clips vertically, so the focus ring needs
        // room of its own or it renders as two arcs.
        py: "4px",
      }}
    >
        {dots.map((i) => {
          const done = i < current;
          const active = i === current;
          const step = steps?.[i];
          const state = done ? "done" : active ? "in progress" : "pending";
          const position = `Step ${i + 1} of ${total} · ${state}`;
          const dot = (
            <Box
              // Labelled dots take focus so the description is reachable
              // without a pointer; bare ones carry nothing to reveal.
              tabIndex={step ? 0 : undefined}
              aria-label={
                step && `${step.label}. ${step.description} ${position}`
              }
              sx={{
                flexShrink: 0,
                width: 18,
                height: 18,
                borderRadius: "50%",
                display: "grid",
                placeItems: "center",
                fontFamily: "var(--font-mono)",
                fontSize: 10,
                fontWeight: 700,
                ...(done && {
                  bgcolor: alpha(theme.palette.success.main, 0.16),
                  color: "success.main",
                  border: `1.5px solid ${theme.palette.success.main}`,
                }),
                ...(active && {
                  bgcolor: "success.main",
                  animation: `${pulse} 1.4s ease-in-out infinite`,
                  [REDUCED]: { animation: "none" },
                }),
                ...(!done &&
                  !active && {
                    border: "1.5px solid",
                    borderColor: "divider",
                  }),
                "&:focus-visible": {
                  outline: "2px solid",
                  outlineColor: "primary.main",
                  outlineOffset: 1,
                },
              }}
            >
              {done ? "✓" : null}
            </Box>
          );
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
              {step ? (
                <Tooltip
                  title={
                    <>
                      <Typography sx={{ fontSize: 12, fontWeight: 700 }}>
                        {step.label}
                      </Typography>
                      <Typography sx={{ fontSize: 12 }}>
                        {step.description}
                      </Typography>
                      <Typography sx={{ fontSize: 11, opacity: 0.7, mt: 0.5 }}>
                        {position}
                      </Typography>
                    </>
                  }
                >
                  {dot}
                </Tooltip>
              ) : (
                dot
              )}
            </Fragment>
          );
        })}
    </Box>
  );
};
