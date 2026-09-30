import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import { PartyList } from "./PartyList";
import type { DecentralizedParty } from "../types";

const party = (name: string): DecentralizedParty => ({
  party_id: `${name}::1220${"a".repeat(64)}`,
  threshold: 1,
  owners: [],
  participants: [],
  contracts: [],
});

const renderList = (starred: string[] = []) => {
  const onSelectParty = vi.fn();
  const onToggleFavorite = vi.fn();
  const onToggleHidden = vi.fn();
  render(
    <SnackbarProvider>
      <PartyList
        parties={[party("alpha"), party("beta")]}
        authStatuses={[]}
        onSelectParty={onSelectParty}
        isHidden={() => false}
        onToggleHidden={onToggleHidden}
        isFavorite={(id) => starred.some((s) => id.startsWith(`${s}::`))}
        onToggleFavorite={onToggleFavorite}
      />
    </SnackbarProvider>,
  );
  return { onSelectParty, onToggleFavorite, onToggleHidden };
};

describe("PartyList stars", () => {
  it("stars a party without opening it", () => {
    const { onSelectParty, onToggleFavorite } = renderList();
    fireEvent.click(screen.getAllByRole("button", { name: "Star party" })[1]);
    expect(onToggleFavorite).toHaveBeenCalledWith(party("beta").party_id);
    expect(onSelectParty).not.toHaveBeenCalled();
  });

  it("shows which parties are starred", () => {
    renderList(["alpha"]);
    const unstar = screen.getByRole("button", { name: "Unstar party" });
    expect(unstar.getAttribute("aria-pressed")).toBe("true");
    expect(screen.getAllByRole("button", { name: "Star party" })).toHaveLength(1);
  });

  it("keeps Enter on a row's buttons from opening the party", () => {
    const { onSelectParty } = renderList();
    fireEvent.keyDown(screen.getAllByRole("button", { name: "Star party" })[0], { key: "Enter" });
    fireEvent.keyDown(screen.getAllByRole("button", { name: "Hide party" })[0], { key: "Enter" });
    expect(onSelectParty).not.toHaveBeenCalled();
    // Enter on the row itself still opens it.
    fireEvent.keyDown(screen.getByRole("button", { name: `Open party ${party("alpha").party_id}` }), { key: "Enter" });
    expect(onSelectParty).toHaveBeenCalledWith(party("alpha").party_id);
  });
});
