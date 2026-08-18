import { Box } from "@mui/material";
import type { ReactNode } from "react";

/**
 * One row of a card list: a bordered, paper-filled strip that reads as a single
 * line. Shared by the parties lists and the approvals feed's Completed section
 * so the three stay the same shape.
 */
export const RowCard = ({
  children,
  onActivate,
  dimmed = false,
  ariaLabel,
  dataAttrs,
}: {
  children: ReactNode;
  /** Makes the whole row a control (pointer, hover, Enter/Space). */
  onActivate?: () => void;
  /** Hidden parties stay readable but recede. */
  dimmed?: boolean;
  ariaLabel?: string;
  /** `data-*` hooks for the e2e suite to select by. */
  dataAttrs?: Record<string, string>;
}) => (
  <Box
    {...dataAttrs}
    role={onActivate ? "button" : undefined}
    tabIndex={onActivate ? 0 : undefined}
    aria-label={ariaLabel}
    onClick={onActivate}
    onKeyDown={
      onActivate
        ? (e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              onActivate();
            }
          }
        : undefined
    }
    sx={{
      display: "flex",
      alignItems: "center",
      gap: 2,
      p: "9px 16px",
      // One height for every card list, whatever a row happens to carry: the
      // parties rows hold 30px icon buttons while the external-parties rows are
      // text only, and left alone the two lists would sit at different heights.
      minHeight: 48,
      border: "1px solid",
      borderColor: "divider",
      borderRadius: "8px",
      bgcolor: "background.paper",
      opacity: dimmed ? 0.45 : 1,
      transition: "border-color 0.15s ease-out, opacity 0.15s ease-out",
      ...(onActivate && {
        cursor: "pointer",
        "&:hover": {
          borderColor: (t) => (t.palette.mode === "dark" ? "#3A332E" : "#C9C2BE"),
        },
        "&:focus-visible": {
          outline: "2px solid var(--accent)",
          outlineOffset: "2px",
        },
      }),
    }}
  >
    {children}
  </Box>
);
