import { Box, IconButton, Typography } from "@mui/material";
import ChevronLeftIcon from "@mui/icons-material/ChevronLeft";
import ChevronRightIcon from "@mui/icons-material/ChevronRight";
import { PAGE_SIZE } from "../constants";

const controlsSx = {
  display: "flex",
  alignItems: "center",
  justifyContent: "flex-end",
  gap: 1,
  pt: 1,
};

/** Range and prev/next for a client-side paged list, from `usePagination`. */
export const PaginationControls = ({
  page,
  pageCount,
  total,
  onChange,
}: {
  page: number;
  pageCount: number;
  total: number;
  onChange: (page: number) => void;
}) => {
  if (total <= PAGE_SIZE) return null;

  const first = page * PAGE_SIZE + 1;
  const last = Math.min(total, (page + 1) * PAGE_SIZE);

  return (
    <Box sx={controlsSx}>
      <Typography variant="caption" color="text.secondary">
        {first}–{last} of {total}
      </Typography>
      <IconButton
        size="small"
        aria-label="Previous page"
        disabled={page === 0}
        onClick={() => onChange(page - 1)}
      >
        <ChevronLeftIcon fontSize="small" />
      </IconButton>
      <IconButton
        size="small"
        aria-label="Next page"
        disabled={page >= pageCount - 1}
        onClick={() => onChange(page + 1)}
      >
        <ChevronRightIcon fontSize="small" />
      </IconButton>
    </Box>
  );
};

/**
 * Prev/next for a cursor-paged endpoint, where the total is unknown — the
 * server only says whether an older page exists.
 */
export const CursorPagination = ({
  page,
  hasNext,
  disabled,
  onPrev,
  onNext,
}: {
  page: number;
  hasNext: boolean;
  disabled?: boolean;
  onPrev: () => void;
  onNext: () => void;
}) => {
  if (page === 0 && !hasNext) return null;

  return (
    <Box sx={controlsSx}>
      <Typography variant="caption" color="text.secondary">
        Page {page + 1}
      </Typography>
      <IconButton
        size="small"
        aria-label="Previous page"
        disabled={disabled || page === 0}
        onClick={onPrev}
      >
        <ChevronLeftIcon fontSize="small" />
      </IconButton>
      <IconButton
        size="small"
        aria-label="Next page"
        disabled={disabled || !hasNext}
        onClick={onNext}
      >
        <ChevronRightIcon fontSize="small" />
      </IconButton>
    </Box>
  );
};
