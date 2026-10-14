import { fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import { PartyList } from "./PartyList";
import type { DecentralizedParty, FormerParty, PurgeStatus } from "../types";

const party = (name: string): DecentralizedParty => ({
  party_id: `${name}::1220${"a".repeat(64)}`,
  threshold: 1,
  owners: [],
  participants: [],
  contracts: [],
});

const former = (name: string, purge_status: PurgeStatus): FormerParty => ({
  party_id: `${name}::1220${"b".repeat(64)}`,
  removed_at: 1_791_000_000,
  detected_at: 1_791_000_060,
  purge_status,
});

const renderList = (
  starred: string[] = [],
  formerParties: FormerParty[] = [],
  parties = [party("alpha"), party("beta")],
) => {
  const onSelectParty = vi.fn();
  const onToggleFavorite = vi.fn();
  const onToggleHidden = vi.fn();
  render(
    <SnackbarProvider>
      <PartyList
        parties={parties}
        authStatuses={[]}
        onSelectParty={onSelectParty}
        isHidden={() => false}
        onToggleHidden={onToggleHidden}
        isFavorite={(id) => starred.some((s) => id.startsWith(`${s}::`))}
        onToggleFavorite={onToggleFavorite}
        formerParties={formerParties}
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

  it("shows which parties are starred through aria-pressed, not the label", () => {
    renderList(["alpha"]);
    const [alpha, beta] = screen.getAllByRole("button", { name: "Star party" });
    expect(alpha.getAttribute("aria-pressed")).toBe("true");
    expect(beta.getAttribute("aria-pressed")).toBe("false");
  });

  it("leaves Enter and Space on a row's buttons to the buttons", () => {
    const { onSelectParty } = renderList();
    const buttons = [
      screen.getAllByRole("button", { name: "Star party" })[0],
      screen.getAllByRole("button", { name: "Hide party" })[0],
      screen.getAllByRole("button", { name: "Copy party id" })[0],
    ];
    for (const button of buttons) {
      // true: the row didn't prevent the default, so the button still clicks.
      expect(fireEvent.keyDown(button, { key: "Enter" })).toBe(true);
      expect(fireEvent.keyDown(button, { key: " " })).toBe(true);
    }
    expect(onSelectParty).not.toHaveBeenCalled();
    // Enter on the row itself still opens it.
    fireEvent.keyDown(screen.getByRole("button", { name: `Open party ${party("alpha").party_id}` }), { key: "Enter" });
    expect(onSelectParty).toHaveBeenCalledWith(party("alpha").party_id);
  });

  it("lets Escape on a row's buttons reach the document, where tooltips close", () => {
    renderList();
    const onDocumentKey = vi.fn();
    document.addEventListener("keydown", onDocumentKey);
    fireEvent.keyDown(screen.getAllByRole("button", { name: "Star party" })[0], { key: "Escape" });
    fireEvent.keyDown(screen.getAllByRole("button", { name: "Hide party" })[0], { key: "Escape" });
    document.removeEventListener("keydown", onDocumentKey);
    expect(onDocumentKey).toHaveBeenCalledTimes(2);
  });
});

describe("PartyList former parties", () => {
  const formers = [
    former("gone", "none"),
    former("purging", "running"),
    former("broken", "failed"),
    former("done", "succeeded"),
  ];

  it("lists them after the hosted parties, each with its purge status", () => {
    renderList([], formers);
    const rows = screen.getAllByTestId("former-party-row");
    expect(rows).toHaveLength(4);

    const lastHosted = screen.getByRole("button", {
      name: `Open party ${party("beta").party_id}`,
    });
    const caption = screen.getByText("No longer hosted by this node");
    expect(
      lastHosted.compareDocumentPosition(caption) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(
      caption.compareDocumentPosition(rows[0]) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();

    const chips: [string, string][] = [
      ["Removed", "MuiChip-colorWarning"],
      ["Purging…", "MuiChip-colorInfo"],
      ["Purge failed", "MuiChip-colorError"],
      ["Purged", "MuiChip-colorSuccess"],
    ];
    rows.forEach((row, i) => {
      const [label, colorClass] = chips[i];
      expect(row.textContent).toContain(formers[i].party_id.split("::")[0]);
      const chip = within(row).getByText(label).closest(".MuiChip-root");
      expect(chip?.classList.contains(colorClass)).toBe(true);
    });
    const purged = within(rows[3]).getByText("Purged").closest(".MuiChip-root");
    expect(purged?.classList.contains("MuiChip-outlined")).toBe(true);
  });

  it("opens a former party through the select callback", () => {
    const { onSelectParty } = renderList([], formers);
    const gone = formers[0].party_id;
    fireEvent.click(
      screen.getByRole("button", { name: `Open former party ${gone}` }),
    );
    expect(onSelectParty).toHaveBeenCalledWith(gone);

    const done = formers[3].party_id;
    fireEvent.keyDown(
      screen.getByRole("button", { name: `Open former party ${done}` }),
      { key: "Enter" },
    );
    expect(onSelectParty).toHaveBeenLastCalledWith(done);
  });

  it("gives former rows no star or hide controls", () => {
    renderList([], formers);
    for (const row of screen.getAllByTestId("former-party-row")) {
      expect(within(row).queryByRole("button", { name: "Star party" })).toBeNull();
      expect(within(row).queryByRole("button", { name: /hide party/i })).toBeNull();
    }
    // Only the two hosted rows carry them.
    expect(screen.getAllByRole("button", { name: "Star party" })).toHaveLength(2);
    expect(screen.getAllByRole("button", { name: "Hide party" })).toHaveLength(2);
  });

  it("still shows former parties when no hosted party matches", () => {
    renderList([], [former("gone", "none")], []);
    expect(screen.getByText("No parties found")).toBeTruthy();
    expect(screen.getAllByTestId("former-party-row")).toHaveLength(1);
  });

  it("shows no former section when there are none", () => {
    renderList();
    expect(screen.queryByText("No longer hosted by this node")).toBeNull();
    expect(screen.queryAllByTestId("former-party-row")).toHaveLength(0);
  });
});
