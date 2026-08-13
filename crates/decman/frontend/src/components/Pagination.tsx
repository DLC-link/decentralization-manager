import { Box, IconButton, Pagination, Tooltip, Typography } from "@mui/material";
import type { SxProps } from "@mui/material";
import { alpha, type Theme } from "@mui/material/styles";
import ChevronLeftIcon from "@mui/icons-material/ChevronLeft";
import ChevronRightIcon from "@mui/icons-material/ChevronRight";
import { PAGE_SIZE } from "../constants";

// Numerics are monospaced so page numbers and ranges don't reflow as digits
// change width. Uses the design system's mono token (Roboto Mono) so page
// numbers match the ids and amounts in the rows above.
const MONO = "var(--font-mono)";

/**
 * Footer bar the table sits on: top rule, range on the left, controls right.
 *
 * Horizontal padding is overridable because the containers this drops into pad
 * their content differently — the bar has to line up with the rows above it.
 */
const footerSx = {
  display: "flex",
  alignItems: "center",
  justifyContent: "space-between",
  flexWrap: "wrap",
  gap: 1.5,
  px: 2,
  py: 1.25,
  mt: 0.5,
  borderTop: 1,
  borderColor: "divider",
  // Pinned to the bottom of whatever scrolls. Pages differ in height — the
  // last one is usually short — so a static footer jumps out of view the
  // moment you change page and has to be chased back down. Needs an opaque
  // fill so rows don't show through it while they scroll past. A no-op in
  // containers that don't scroll (the Packages panel already pins it).
  position: "sticky",
  bottom: 0,
  zIndex: 2,
  bgcolor: "background.paper",
};

const rangeSx = {
  fontFamily: MONO,
  fontSize: "0.75rem",
  letterSpacing: "0.02em",
  color: "text.secondary",
  fontVariantNumeric: "tabular-nums",
};

/**
 * Pill styling for the page items.
 *
 * The selected page is an accent *tint* with accent text rather than a solid
 * accent fill: the brand's action orange doesn't clear AA against white at this
 * size, so white-on-orange numerals would be unreadable.
 */
const paginationSx = {
  "& .MuiPagination-ul": { flexWrap: "nowrap" },
  "& .MuiPaginationItem-root": {
    fontFamily: MONO,
    fontSize: "0.75rem",
    fontVariantNumeric: "tabular-nums",
    minWidth: 30,
    height: 30,
    margin: 0,
    marginInline: "2px",
    borderRadius: "6px",
    color: "text.secondary",
    transition:
      "background-color 150ms ease-out, border-color 150ms ease-out, color 150ms ease-out",
    "&:hover": { backgroundColor: "action.hover" },
  },
  "& .MuiPaginationItem-root.Mui-selected": {
    backgroundColor: (theme: Theme) =>
      alpha(theme.palette.primary.main, 0.14),
    border: 1,
    borderColor: (theme: Theme) =>
      alpha(theme.palette.primary.main, 0.4),
    color: "primary.main",
    fontWeight: 600,
    "&:hover": {
      backgroundColor: (theme: Theme) =>
        alpha(theme.palette.primary.main, 0.22),
    },
  },
  "& .MuiPaginationItem-ellipsis": {
    height: 30,
    lineHeight: "30px",
    color: "text.disabled",
  },
};

/** Range and page pills for a client-side paged list, from `usePagination`. */
export const PaginationControls = ({
  page,
  pageCount,
  total,
  onChange,
  sx,
}: {
  page: number;
  pageCount: number;
  total: number;
  onChange: (page: number) => void;
  sx?: SxProps<Theme>;
}) => {
  if (total <= PAGE_SIZE) return null;

  const first = page * PAGE_SIZE + 1;
  const last = Math.min(total, (page + 1) * PAGE_SIZE);

  return (
    <Box sx={[footerSx, ...(Array.isArray(sx) ? sx : [sx])]}>
      <Typography sx={rangeSx}>
        {first}–{last} of {total}
      </Typography>
      <Pagination
        count={pageCount}
        page={page + 1}
        onChange={(_, next) => onChange(next - 1)}
        size="small"
        shape="rounded"
        siblingCount={1}
        boundaryCount={1}
        sx={paginationSx}
      />
    </Box>
  );
};

const stepSx = {
  width: 30,
  height: 30,
  borderRadius: "6px",
  border: 1,
  borderColor: "divider",
  color: "text.secondary",
  transition: "background-color 150ms ease-out, border-color 150ms ease-out",
  "&:hover": { backgroundColor: "action.hover", borderColor: "text.disabled" },
  "&.Mui-disabled": { opacity: 0.4, borderColor: "divider" },
};

/**
 * Prev/next for a cursor-paged endpoint. The total is unknown — the server only
 * says whether an older page exists — so there are no page numbers to render.
 */
export const CursorPagination = ({
  page,
  hasNext,
  disabled,
  onPrev,
  onNext,
  sx,
}: {
  page: number;
  hasNext: boolean;
  disabled?: boolean;
  onPrev: () => void;
  onNext: () => void;
  sx?: SxProps<Theme>;
}) => {
  if (page === 0 && !hasNext) return null;

  const atStart = disabled || page === 0;
  const atEnd = disabled || !hasNext;

  return (
    <Box sx={[footerSx, ...(Array.isArray(sx) ? sx : [sx])]}>
      <Typography sx={rangeSx}>Page {page + 1}</Typography>
      <Box sx={{ display: "flex", gap: 0.5 }}>
        <Tooltip title={atStart ? "" : "Newer"}>
          <span>
            <IconButton
              size="small"
              aria-label="Newer entries"
              disabled={atStart}
              onClick={onPrev}
              sx={stepSx}
            >
              <ChevronLeftIcon fontSize="small" />
            </IconButton>
          </span>
        </Tooltip>
        <Tooltip title={atEnd ? "" : "Older"}>
          <span>
            <IconButton
              size="small"
              aria-label="Older entries"
              disabled={atEnd}
              onClick={onNext}
              sx={stepSx}
            >
              <ChevronRightIcon fontSize="small" />
            </IconButton>
          </span>
        </Tooltip>
      </Box>
    </Box>
  );
};
