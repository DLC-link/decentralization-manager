/** Zebra stripe sx for table rows — subtle alternating background and orange hover tint */
export const zebraRow = (index: number) => ({
  bgcolor: index % 2 === 0 ? "transparent" : "action.hover",
  "&:hover td": {
    backgroundColor: "rgba(255, 102, 51, 0.08)",
    transition: "background-color 0.15s ease",
  },
});
