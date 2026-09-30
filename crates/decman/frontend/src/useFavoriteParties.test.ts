import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";

import { favoritesFirst, useFavoriteParties } from "./useFavoriteParties";

const parties = ["a", "b", "c", "d"].map((party_id) => ({ party_id }));

describe("favoritesFirst", () => {
  it("moves starred parties to the top and keeps both groups in order", () => {
    const starred = new Set(["d", "b"]);
    expect(favoritesFirst(parties, (id) => starred.has(id)).map((p) => p.party_id)).toEqual([
      "b",
      "d",
      "a",
      "c",
    ]);
  });

  it("leaves the list as it is when nothing is starred", () => {
    expect(favoritesFirst(parties, () => false)).toBe(parties);
  });
});

describe("useFavoriteParties", () => {
  beforeEach(() => localStorage.clear());

  it("toggles a party and keeps the stars across a reload", () => {
    const first = renderHook(() => useFavoriteParties());
    act(() => first.result.current.toggle("b"));
    expect(first.result.current.isFavorite("b")).toBe(true);
    expect(JSON.parse(localStorage.getItem("favorite-parties")!)).toEqual(["b"]);

    const reloaded = renderHook(() => useFavoriteParties());
    expect(reloaded.result.current.isFavorite("b")).toBe(true);

    act(() => reloaded.result.current.toggle("b"));
    expect(reloaded.result.current.isFavorite("b")).toBe(false);
    expect(JSON.parse(localStorage.getItem("favorite-parties")!)).toEqual([]);
  });

  it("starts empty when the stored value is unreadable", () => {
    localStorage.setItem("favorite-parties", "not json");
    const { result } = renderHook(() => useFavoriteParties());
    expect(result.current.favorites.size).toBe(0);
  });
});
