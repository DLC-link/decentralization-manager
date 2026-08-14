import type { Theme } from "@mui/material/styles";

/**
 * The card column: a capped, centered reading width, matching the one the
 * approvals feed already uses so the two views read at the same measure.
 *
 * Rows live in here; a list's footer bar is a sibling *outside* it, so the
 * footer rule runs the full width of the view under content that stops short of
 * it — the way the tables these lists replaced behaved.
 */
export const COLUMN_MAX_WIDTH = 1040;

export const columnSx = {
  maxWidth: COLUMN_MAX_WIDTH,
  mx: "auto",
  // Keeps rows off the edges once the viewport is narrower than the cap.
  px: 3,
};

/** Column label above a card list, in the same treatment as a table head. */
export const legendSx = {
  fontFamily: "var(--font-mono)",
  fontSize: "0.7rem",
  fontWeight: 500,
  letterSpacing: "0.12em",
  textTransform: "uppercase" as const,
  color: "text.secondary",
};

/** Zebra stripe sx for table rows — subtle alternating background and accent hover tint */
export const zebraRow = (index: number) => ({
  bgcolor: (theme: Theme) =>
    index % 2 === 0
      ? "transparent"
      : theme.palette.mode === "dark"
        ? // The near-black dark substrate makes the default action.hover (8%
          // white) read as a harsh stripe — keep the alternation barely-there.
          "rgba(255, 255, 255, 0.025)"
        : theme.palette.action.hover,
  "&:hover td": {
    backgroundColor: "rgba(214, 58, 15, 0.08)",
    transition: "background-color 0.15s ease-out",
  },
});
