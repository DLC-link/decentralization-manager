import { useMemo, useState } from "react";
import { PAGE_SIZE } from "./constants";

/**
 * Slice a fully-loaded list into PAGE_SIZE pages.
 *
 * Used by the views whose endpoint returns the whole result set. The backend
 * still reads Canton a page at a time; it just can't hand out a cursor for a
 * list it assembles from several template queries, so the paging surfaces here
 * instead. Views backed by a cursor endpoint use `CursorPagination`.
 */
export function usePagination<T>(items: T[]) {
  const [requestedPage, setPage] = useState(0);

  const pageCount = Math.max(1, Math.ceil(items.length / PAGE_SIZE));
  // Clamped while rendering rather than corrected in an effect: when the list
  // shrinks under the current page (a filter is typed, a refresh returns
  // fewer rows) the view lands on the last real page immediately, with no
  // intermediate render showing an empty one.
  const page = Math.min(requestedPage, pageCount - 1);

  const pageItems = useMemo(
    () => items.slice(page * PAGE_SIZE, page * PAGE_SIZE + PAGE_SIZE),
    [items, page],
  );

  return { page, setPage, pageCount, pageItems, total: items.length };
}
