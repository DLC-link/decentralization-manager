import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import { HostingParticipant } from "./HostingParticipant";
import type { Peer } from "../types";

const ns = "a".repeat(68);
const bravo = `bravo::${ns}`;
const charlie = `charlie::${ns}`;

const peers: Peer[] = [
  { participant_id: bravo, name: "Bravo", address: "b", port: 1, public_key: "" },
];

const renderCell = (
  props: Partial<Parameters<typeof HostingParticipant>[0]> = {},
) => {
  const onOpenPeerConfig = vi.fn();
  const onShowSharedParties = vi.fn();
  render(
    <HostingParticipant
      participantId={bravo}
      isSelf={false}
      peers={peers}
      sharedCount={4}
      onOpenPeerConfig={onOpenPeerConfig}
      onShowSharedParties={onShowSharedParties}
      {...props}
    />,
    { wrapper: SnackbarProvider },
  );
  return { onOpenPeerConfig, onShowSharedParties };
};

describe("HostingParticipant", () => {
  it("links a configured peer's name to its configuration row", () => {
    const { onOpenPeerConfig } = renderCell();

    fireEvent.click(screen.getByRole("button", { name: "Bravo" }));

    expect(onOpenPeerConfig).toHaveBeenCalledWith(bravo);
  });

  it("opens the shared parties from the count chip", () => {
    const { onShowSharedParties } = renderCell();

    fireEvent.click(screen.getByRole("button", { name: "4 shared" }));

    expect(onShowSharedParties).toHaveBeenCalledWith(bravo);
  });

  it("names a host that is not a configured peer, without a link", () => {
    renderCell({ participantId: charlie });

    expect(screen.getByText("Not a configured peer")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /charlie/i })).toBeNull();
    // Shared parties come from the topology, not the peer list.
    expect(screen.getByRole("button", { name: "4 shared" })).toBeTruthy();
  });

  it("marks this node and gives it neither a link nor a count", () => {
    renderCell({ isSelf: true });

    expect(screen.getByText("Bravo (You)")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Bravo" })).toBeNull();
    expect(screen.queryByText(/shared/)).toBeNull();
  });

  it("disables the chip for a participant with no shared party", () => {
    const { onShowSharedParties } = renderCell({ sharedCount: 0 });

    const chip = screen.getByRole("button", { name: "0 shared" });
    expect(chip.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(chip);
    expect(onShowSharedParties).not.toHaveBeenCalled();
  });

  it("shows only the id while the peers and counts load", () => {
    renderCell({ peers: null, sharedCount: null });

    expect(screen.queryByText("Not a configured peer")).toBeNull();
    expect(screen.queryByText(/shared/)).toBeNull();
    expect(screen.getByText(/^bravo::/)).toBeTruthy();
  });
});
