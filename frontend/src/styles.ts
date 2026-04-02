/** Zebra stripe sx for table rows — subtle alternating background like Apple lists */
export const zebraRow = (index: number) => ({
  bgcolor: index % 2 === 0 ? "transparent" : "action.hover",
});
