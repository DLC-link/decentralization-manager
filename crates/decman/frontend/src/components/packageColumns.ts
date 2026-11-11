// Comparison-table columns. Every one is stated, because a fixed-layout table
// hands an unstated column a share of the space rather than the remainder, which
// had left the package name in ~110px. Peers share one width so they read as a
// grid however long their names are, and adding peers pushes the table wider —
// into the scroller — instead of squeezing the name that identifies the row.
const PEER_COL_WIDTH = 150;
// Fits a Canton build version such as 3.4.0.20251020.14338.0 on one line.
const VERSION_COL_WIDTH = 176;
const PACKAGE_MIN_WIDTH = 260;
// Local table: the id column fits the truncated id and its copy button, and the
// name takes what is left, wrapping when it has to. Declared widths keep a
// 64-character id or a long version from widening the table past the panel.
// 352 fits the widest form: from md up, CopyableText shows the id as 16…16
// characters plus the button.
const PACKAGE_ID_COL_WIDTH = 352;
const LOCAL_NAME_MIN_WIDTH = 220;
const LOCAL_TABLE_MIN_WIDTH =
  LOCAL_NAME_MIN_WIDTH + VERSION_COL_WIDTH + PACKAGE_ID_COL_WIDTH;

export { PEER_COL_WIDTH, VERSION_COL_WIDTH, PACKAGE_MIN_WIDTH, PACKAGE_ID_COL_WIDTH, LOCAL_TABLE_MIN_WIDTH };
