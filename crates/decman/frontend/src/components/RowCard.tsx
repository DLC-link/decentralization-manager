import { Box, Collapse } from "@mui/material";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import { useState, type ReactNode } from "react";
import { EXPANDER_SLOT } from "../styles";

/**
 * One row of a card list: a bordered, paper-filled strip that reads as a single
 * line. Shared by the parties lists and the approvals feed's Completed section
 * so the three stay the same shape.
 *
 * A row is either a control (`onActivate` — the parties list opens a party) or
 * an expander (`detail` — the external-parties list reveals its hosts), never
 * both: one click can't mean two things.
 */
export const RowCard = ({
  children,
  onActivate,
  detail,
  detailLabel,
  dimmed = false,
  ariaLabel,
  dataAttrs,
}: {
  children: ReactNode;
  /** Makes the whole row a control (pointer, hover, Enter/Space). */
  onActivate?: () => void;
  /** Revealed below the row when it is expanded. Makes the row an expander. */
  detail?: ReactNode;
  /** Accessible name for the expander, when `detail` is set. */
  detailLabel?: string;
  /** Hidden parties stay readable but recede. */
  dimmed?: boolean;
  ariaLabel?: string;
  /** `data-*` hooks for the e2e suite to select by. */
  dataAttrs?: Record<string, string>;
}) => {
  const [open, setOpen] = useState(false);
  const expandable = detail != null;
  const activate = expandable ? () => setOpen((o) => !o) : onActivate;
  return (
    <Box
      {...dataAttrs}
      sx={{
        border: "1px solid",
        borderColor: "divider",
        borderRadius: "8px",
        bgcolor: "background.paper",
        opacity: dimmed ? 0.45 : 1,
        transition:
          "border-color 0.15s ease-out, background-color 0.15s ease-out, opacity 0.15s ease-out",
        // Every row lifts on hover, whether or not it is clickable: the lists
        // are read across, and the tint is what tracks the eye along one row.
        "&:hover": {
          borderColor: (t) => (t.palette.mode === "dark" ? "#3A332E" : "#C9C2BE"),
          bgcolor: (t) =>
            t.palette.mode === "dark"
              ? "rgba(255, 255, 255, 0.035)"
              : "rgba(0, 0, 0, 0.022)",
        },
        "&:focus-within": {
          borderColor: (t) => (t.palette.mode === "dark" ? "#3A332E" : "#C9C2BE"),
        },
      }}
    >
      <Box
        role={activate ? "button" : undefined}
        tabIndex={activate ? 0 : undefined}
        aria-label={expandable ? detailLabel : ariaLabel}
        aria-expanded={expandable ? open : undefined}
        onClick={activate}
        onKeyDown={
          activate
            ? (e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  activate();
                }
              }
            : undefined
        }
        sx={{
          display: "flex",
          alignItems: "center",
          gap: 2,
          p: "9px 16px",
          borderRadius: "8px",
          // One height for every card list, whatever a row happens to carry: the
          // parties rows hold 30px icon buttons while the external-parties rows are
          // text only, and left alone the two lists would sit at different heights.
          minHeight: 48,
          ...(activate && { cursor: "pointer" }),
          "&:focus-visible": {
            outline: "2px solid var(--accent)",
            outlineOffset: "-2px",
          },
        }}
      >
        {expandable && (
          <ExpandMoreIcon
            sx={{
              fontSize: EXPANDER_SLOT,
              width: EXPANDER_SLOT,
              flexShrink: 0,
              color: "text.disabled",
              transform: open ? "rotate(180deg)" : "rotate(0deg)",
              transition: "transform 0.2s ease",
            }}
          />
        )}
        {children}
      </Box>
      {expandable && (
        <Collapse in={open} timeout="auto" unmountOnExit>
          {detail}
        </Collapse>
      )}
    </Box>
  );
};
