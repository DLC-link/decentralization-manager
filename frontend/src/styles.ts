import type { Theme } from "@mui/material/styles";

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
