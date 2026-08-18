import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { PAGE_SIZE } from "./constants";
import { usePagination } from "./usePagination";

const rows = (count: number): number[] =>
  Array.from({ length: count }, (_, i) => i);

describe("usePagination", () => {
  it("slices the list into PAGE_SIZE pages", () => {
    const items = rows(PAGE_SIZE * 2 + 3);
    const { result } = renderHook(() => usePagination(items));

    expect(result.current.pageCount).toBe(3);
    expect(result.current.total).toBe(items.length);
    expect(result.current.pageItems).toEqual(items.slice(0, PAGE_SIZE));

    act(() => result.current.setPage(2));
    expect(result.current.page).toBe(2);
    expect(result.current.pageItems).toEqual(items.slice(PAGE_SIZE * 2));
  });

  it("reports one page for an empty list", () => {
    const { result } = renderHook(() => usePagination([]));

    expect(result.current.pageCount).toBe(1);
    expect(result.current.page).toBe(0);
    expect(result.current.pageItems).toEqual([]);
  });

  // The clamp is what keeps a shrinking list (a filter typed, a refresh that
  // returns fewer rows) from rendering an empty page.
  it("clamps to the last page when the list shrinks", () => {
    const { result, rerender } = renderHook(
      ({ items }) => usePagination(items),
      { initialProps: { items: rows(PAGE_SIZE * 3) } },
    );

    act(() => result.current.setPage(2));
    expect(result.current.page).toBe(2);

    rerender({ items: rows(PAGE_SIZE + 1) });
    expect(result.current.pageCount).toBe(2);
    expect(result.current.page).toBe(1);
  });

  // The requested page has to be forgotten, not just clamped: if the list grows
  // back, the view must stay where the clamp left it rather than jumping to a
  // page the user last asked for before the shrink.
  it("does not jump back to a stale page when the list regrows", () => {
    const { result, rerender } = renderHook(
      ({ items }) => usePagination(items),
      { initialProps: { items: rows(PAGE_SIZE * 3) } },
    );

    act(() => result.current.setPage(2));
    rerender({ items: rows(1) });
    expect(result.current.page).toBe(0);

    rerender({ items: rows(PAGE_SIZE * 3) });
    expect(result.current.pageCount).toBe(3);
    expect(result.current.page).toBe(0);
  });

  it("returns to the first row of the scroll container on a page change", () => {
    const container = document.createElement("div");
    container.scrollTo = vi.fn();

    const { result } = renderHook(() =>
      usePagination(rows(PAGE_SIZE * 2), { current: container }),
    );

    act(() => result.current.setPage(1));
    expect(container.scrollTo).toHaveBeenCalledWith({ top: 0 });
  });

  // Without a scroll container the rows scroll with the page itself.
  it("scrolls the window when no container is given", () => {
    const scrollTo = vi.fn();
    window.scrollTo = scrollTo;

    const { result } = renderHook(() => usePagination(rows(PAGE_SIZE * 2)));

    act(() => result.current.setPage(1));
    expect(scrollTo).toHaveBeenCalledWith({ top: 0 });
  });
});
