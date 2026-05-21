import type { ReactNode } from "react";
import { IconButton, InputAdornment, Tooltip } from "@mui/material";
import HelpOutlineIcon from "@mui/icons-material/HelpOutlineRounded";

interface FieldHelpProps {
  /// The tooltip body. Plain English, 1-2 short sentences. Avoid jargon
  /// without explanation. Renders verbatim — no markdown / formatting.
  text: string;
  /// Aria label for the trigger icon. Defaults to "More info"; override
  /// when the tooltip pertains to a specific field to give screen-reader
  /// users a more meaningful announcement (e.g. "Help for Party ID prefix").
  ariaLabel?: string;
}

/// Inline help indicator for form fields and inline labels. Renders a
/// small `?` icon that reveals a tooltip on hover (desktop) / tap
/// (mobile) / keyboard focus, and dismisses on outside click or escape.
///
/// Two intended usages:
///
/// 1. As an `endAdornment` inside a `TextField` slotProps:
///    `slotProps={{ input: { endAdornment: <FieldHelp text="…" /> } }}`
/// 2. Inline next to a label / column header — drop the component as a
///    sibling of the label text.
///
/// MUI's `Tooltip` already handles the accessibility wiring
/// (keyboard-focusable, `aria-describedby` on the wrapped element,
/// escape-to-dismiss); we just supply the trigger.
export const FieldHelp = ({ text, ariaLabel }: FieldHelpProps) => (
  <Tooltip title={text} placement="top" arrow enterTouchDelay={0}>
    <IconButton
      size="small"
      aria-label={ariaLabel ?? "More info"}
      sx={{ p: 0.25, color: "text.secondary" }}
      // Prevent the icon click from triggering a form submit when nested
      // inside a `<form>`. The tooltip handles hover/focus opening on its
      // own; the button click is only useful on touch devices, and even
      // then MUI's `enterTouchDelay={0}` opens the tooltip without needing
      // a real click.
      type="button"
      onClick={(e) => e.preventDefault()}
    >
      <HelpOutlineIcon fontSize="small" />
    </IconButton>
  </Tooltip>
);

/// Convenience: a ready-made `endAdornment` for a `TextField`. Use as
/// `slotProps={{ input: { endAdornment: fieldHelpAdornment("...") } }}`.
export const fieldHelpAdornment = (text: string, ariaLabel?: string) => (
  <InputAdornment position="end">
    <FieldHelp text={text} ariaLabel={ariaLabel} />
  </InputAdornment>
);

interface TextHelpProps {
  text: string;
  children: ReactNode;
}

/// Hover-on-text tooltip without an inline `(?)` icon — use for accordion
/// titles, table headers, status pills, and any label-on-non-input surface
/// where the icon would feel cluttered. The wrapped text gets a help
/// cursor and is keyboard-focusable so the tooltip is reachable from
/// keyboard + screen readers.
export const TextHelp = ({ text, children }: TextHelpProps) => (
  <Tooltip title={text} placement="top" arrow enterTouchDelay={0}>
    <span tabIndex={0} style={{ cursor: "help", outline: "none" }}>
      {children}
    </span>
  </Tooltip>
);
