import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";

import { favoritesFirst, useFavoriteParties } from "./useFavoriteParties";

const parties = ["a", "b", "c", "d"].map((party_id) => ({ party_id }));

const ids = (list: { party_id: string }[]) => list.map((p) => p.party_id);

describe("favoritesFirst", () => {
  it("puts starred parties on top in star order, the rest in backend order", () => {
    expect(ids(favoritesFirst(parties, new Set(["d", "b"])))).toEqual(["d", "b", "a", "c"]);
  });

  it("keeps the starred order when the backend reorders the parties", () => {
    const starred = new Set(["d", "b"]);
    const refreshed = [...parties].reverse();
    expect(ids(favoritesFirst(refreshed, starred)).slice(0, 2)).toEqual(["d", "b"]);
    expect(ids(favoritesFirst(parties, starred)).slice(0, 2)).toEqual(["d", "b"]);
  });

  it("leaves the list as it is when no listed party is starred", () => {
    expect(favoritesFirst(parties, new Set())).toBe(parties);
    expect(favoritesFirst(parties, new Set(["gone"]))).toBe(parties);
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
