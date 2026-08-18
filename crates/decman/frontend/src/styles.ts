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
  // Explicit, because these boxes also sit inside flex columns: there the `auto`
  // side margins suppress stretching, and without a width the box would shrink
  // to its content instead of holding the column.
  width: "100%",
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

// Card lists have no header row, so the two icon columns are labelled by a
// legend above them. These widths are shared by the legend, the cards and the
// loading skeleton to keep all three aligned.
export const AUTH_SLOT = 56;
export const VISIBILITY_SLOT = 84;
// Trailing space that holds the visibility toggle out from under the fixed
// Create-Party FAB (56px, offset 24px) in this view's bottom-right corner.
//
// Only earns its keep while the column can actually reach that corner. Once the
// column is capped and centered, the toggle already clears the FAB, and the
// space would just push the icons away from the edge — so it drops out above the
// width where the centering takes over (the column's own 24px padding plus the
// 58px from the card's edge to the toggle exceed the FAB's 80px by then).
export const FAB_GUTTER = 40;

export const fabGutterSx = {
  width: FAB_GUTTER,
  flexShrink: 0,
  "@media (min-width: 1369px)": { width: 0 },
};
