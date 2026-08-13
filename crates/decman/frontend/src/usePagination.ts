import { useCallback, useMemo, useState, type RefObject } from "react";
import { PAGE_SIZE } from "./constants";

/**
 * Slice a fully-loaded list into PAGE_SIZE pages.
 *
 * Used by the views whose endpoint returns the whole result set. The backend
 * still reads Canton a page at a time; it just can't hand out a cursor for a
 * list it assembles from several template queries, so the paging surfaces here
 * instead. Views backed by a cursor endpoint use `CursorPagination`.
 *
 * `scrollRef` is the element that scrolls the rows, when it isn't the window —
 * pass it so changing page can return to the first row.
 */
export function usePagination<T>(
  items: T[],
  scrollRef?: RefObject<HTMLElement | null>,
) {
  const [requestedPage, setRequestedPage] = useState(0);

  const pageCount = Math.max(1, Math.ceil(items.length / PAGE_SIZE));
  // Clamped while rendering rather than corrected in an effect: when the list
  // shrinks under the current page (a filter is typed, a refresh returns
  // fewer rows) the view lands on the last real page immediately, with no
  // intermediate render showing an empty one.
  const page = Math.min(requestedPage, pageCount - 1);

  // The clamp above only hides an out-of-range request; it has to be forgotten
  // too, or a list that shrinks and later grows back jumps to the stale page the
  // user never asked for again. Adjusted while rendering rather than in an
  // effect, which is what keeps the empty page from ever being shown.
  if (requestedPage > page) {
    setRequestedPage(page);
  }

  const setPage = useCallback(
    (next: number) => {
      setRequestedPage(next);
      // Back to the first row of the new page. Two reasons: a page is read from
      // its top, and pages differ in height — the last one is short, so leaving
      // the scroll position alone lets the browser clamp it upward and the
      // header appears to lurch into view.
      if (scrollRef?.current) {
        scrollRef.current.scrollTo({ top: 0 });
      } else {
        window.scrollTo({ top: 0 });
      }
    },
    [scrollRef],
  );

  const pageItems = useMemo(
    () => items.slice(page * PAGE_SIZE, page * PAGE_SIZE + PAGE_SIZE),
    [items, page],
  );

  return { page, setPage, pageCount, pageItems, total: items.length };
}
