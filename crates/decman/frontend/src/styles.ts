import type { Theme } from "@mui/material/styles";

/** Horizontal inset for list content — the column the rest of the UI aligns to. */
export const LIST_INSET = "var(--content-pad)";

/**
 * Cancels {@link LIST_INSET}, for the footer bar that runs the full width of
 * the view while the rows above it stay in the column. The tables these lists
 * replaced did the same thing: the footer rule ran edge to edge under content
 * that stopped short of it.
 */
export const LIST_BLEED = "calc(-1 * var(--content-pad))";

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
