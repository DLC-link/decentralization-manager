import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import { PeersCsvDialog } from "./PeersCsvDialog";
import { peersToCsv } from "../peerCsv";
import type { Peer } from "../types";

const peer = (over: Partial<Peer> = {}): Peer => ({
  participant_id: "a::1220aaaa",
  name: "Alpha",
  address: "alpha.example.com",
  port: 9000,
  public_key: "keyA",
  ...over,
});

const alpha = peer();
const bravo = peer({
  participant_id: "b::1220bbbb",
  name: "Bravo",
  address: "bravo.example.com",
  port: 9001,
  public_key: "keyB",
});

const renderDialog = (props: Partial<Parameters<typeof PeersCsvDialog>[0]> = {}) =>
  render(
    <SnackbarProvider>
      <PeersCsvDialog
        open
        mode="export"
        peers={[alpha, bravo]}
        onClose={() => {}}
        {...props}
      />
    </SnackbarProvider>,
  );

/** Upload `content` as a .csv through the dialog's hidden file input. */
const uploadCsv = (content: string) => {
  const file = new File([content], "peers.csv", { type: "text/csv" });
  const input = document.querySelector<HTMLInputElement>('input[type="file"]');
  if (!input) throw new Error("file input not rendered");
  fireEvent.change(input, { target: { files: [file] } });
};

describe("PeersCsvDialog — export", () => {
  const createObjectURL = vi.fn(() => "blob:peers");
  const revokeObjectURL = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
    // jsdom implements neither, and the download helper calls both.
    URL.createObjectURL = createObjectURL;
    URL.revokeObjectURL = revokeObjectURL;
  });

  it("lists every peer and selects them all by default", () => {
    renderDialog();

    expect(screen.getByText("2 of 2 selected")).toBeDefined();
    for (const box of screen.getAllByRole("checkbox")) {
      expect((box as HTMLInputElement).checked).toBe(true);
    }
  });

  it("downloads only the peers left selected", async () => {
    const clicked: HTMLAnchorElement[] = [];
    vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (
      this: HTMLAnchorElement,
    ) {
      clicked.push(this);
    });
    let blob: Blob | undefined;
    createObjectURL.mockImplementation(((b: Blob) => {
      blob = b;
      return "blob:peers";
    }) as unknown as () => string);

    renderDialog();
    fireEvent.click(screen.getByRole("checkbox", { name: "Select Bravo" }));

    expect(screen.getByText("1 of 2 selected")).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: /Download 1/ }));

    expect(clicked).toHaveLength(1);
    expect(clicked[0]?.download).toMatch(/^decman-peers-\d{4}-\d{2}-\d{2}\.csv$/);
    const text = await blob?.text();
    expect(text).toContain("a::1220aaaa,Alpha");
    expect(text).not.toContain("Bravo");
    expect(revokeObjectURL).toHaveBeenCalledWith("blob:peers");
  });

  // Nothing seeds the selection from props, so a reopened dialog would inherit
  // the last visit's checkboxes if it did not wipe itself on the way in.
  it("starts fresh when reopened after a deselection", async () => {
    const { rerender } = renderDialog();
    const reopen = (open: boolean) =>
      rerender(
        <SnackbarProvider>
          <PeersCsvDialog
            open={open}
            mode="export"
            peers={[alpha, bravo]}
            onClose={() => {}}
          />
        </SnackbarProvider>,
      );

    fireEvent.click(screen.getByRole("checkbox", { name: "Select Bravo" }));
    expect(screen.getByText("1 of 2 selected")).toBeDefined();

    reopen(false);
    reopen(true);

    await waitFor(() => expect(screen.getByText("2 of 2 selected")).toBeDefined());
  });

  it("disables the download button once nothing is selected", async () => {
    renderDialog();

    fireEvent.click(screen.getByRole("checkbox", { name: "Select Alpha" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Select Bravo" }));

    expect(screen.getByText("0 of 2 selected")).toBeDefined();
    expect(
      screen.getByRole("button", { name: /Download 0/ }).getAttribute("disabled"),
    ).not.toBeNull();
  });
});

describe("PeersCsvDialog — import", () => {
  it("asks for a file before showing any rows", () => {
    renderDialog({ mode: "import", onSave: vi.fn() });

    expect(
      screen.getByText("Choose a CSV file to see the peers it contains."),
    ).toBeDefined();
  });

  // The merge is the whole contract of the import: existing peers survive,
  // colliding ones are overwritten, new ones are appended.
  it("merges the selected rows into the existing peers", async () => {
    const onSave = vi.fn().mockResolvedValue(undefined);
    const onClose = vi.fn();
    const charlie = peer({
      participant_id: "c::1220cccc",
      name: "Charlie",
      address: "charlie.example.com",
      port: 9002,
      public_key: "keyC",
    });
    const bravoMoved = { ...bravo, address: "bravo-2.example.com", port: 9100 };

    renderDialog({ mode: "import", onSave, onClose });
    uploadCsv(peersToCsv([bravoMoved, charlie]));

    await waitFor(() => expect(screen.getByText("2 of 2 selected")).toBeDefined());
    fireEvent.click(screen.getByRole("button", { name: /Import 2/ }));

    await waitFor(() => expect(onSave).toHaveBeenCalledTimes(1));
    expect(onSave.mock.calls[0]?.[0]).toEqual([alpha, bravoMoved, charlie]);
    expect(onClose).toHaveBeenCalled();
  });

  it("labels each row as New, Update or Unchanged", async () => {
    const charlie = peer({
      participant_id: "c::1220cccc",
      name: "Charlie",
      address: "charlie.example.com",
      port: 9002,
      public_key: "keyC",
    });

    renderDialog({ mode: "import", onSave: vi.fn() });
    uploadCsv(peersToCsv([alpha, { ...bravo, port: 9100 }, charlie]));

    await waitFor(() => expect(screen.getByText("Charlie")).toBeDefined());
    const kindOf = (name: string) => {
      const row = screen.getByText(name).closest("tr");
      if (!row) throw new Error(`no row for ${name}`);
      return within(row).getByText(/New|Update|Unchanged/).textContent;
    };
    expect(kindOf("Alpha")).toBe("Unchanged");
    expect(kindOf("Bravo")).toBe("Update");
    expect(kindOf("Charlie")).toBe("New");
  });

  it("imports only the rows left selected", async () => {
    const onSave = vi.fn().mockResolvedValue(undefined);
    const charlie = peer({
      participant_id: "c::1220cccc",
      name: "Charlie",
      address: "charlie.example.com",
      port: 9002,
      public_key: "keyC",
    });
    const delta = peer({
      participant_id: "d::1220dddd",
      name: "Delta",
      address: "delta.example.com",
      port: 9003,
      public_key: "keyD",
    });

    renderDialog({ mode: "import", onSave });
    uploadCsv(peersToCsv([charlie, delta]));

    await waitFor(() => expect(screen.getByText("2 of 2 selected")).toBeDefined());
    fireEvent.click(screen.getByRole("checkbox", { name: "Select Delta" }));
    fireEvent.click(screen.getByRole("button", { name: /Import 1/ }));

    await waitFor(() => expect(onSave).toHaveBeenCalledTimes(1));
    expect(onSave.mock.calls[0]?.[0]).toEqual([alpha, bravo, charlie]);
  });

  // The summary counts only what the import would actually write, so an
  // unchanged row and a deselected one must both drop out of it.
  it("summarises how many peers the import would change", async () => {
    const charlie = peer({
      participant_id: "c::1220cccc",
      name: "Charlie",
      address: "charlie.example.com",
      port: 9002,
      public_key: "keyC",
    });

    renderDialog({ mode: "import", onSave: vi.fn() });
    uploadCsv(peersToCsv([alpha, { ...bravo, port: 9100 }, charlie]));

    await waitFor(() =>
      expect(
        screen.getByText(
          "2 peers to add or update. Peers not in the file are kept.",
        ),
      ).toBeDefined(),
    );

    fireEvent.click(screen.getByRole("checkbox", { name: "Select Charlie" }));
    expect(
      screen.getByText(
        "1 peer to add or update. Peers not in the file are kept.",
      ),
    ).toBeDefined();

    fireEvent.click(screen.getByRole("checkbox", { name: "Select Bravo" }));
    expect(
      screen.getByText("Nothing to change — the selected peers already match."),
    ).toBeDefined();
  });

  it("reports the rows it skipped and keeps the good ones", async () => {
    renderDialog({ mode: "import", onSave: vi.fn() });

    uploadCsv(
      [
        "participant_id,name,address,port,public_key,party",
        "c::1220cccc,Charlie,charlie.example.com,9002,keyC,",
        ",Nameless,x.example.com,9000,keyX,",
      ].join("\n"),
    );

    await waitFor(() => expect(screen.getByText("1 row skipped")).toBeDefined());
    expect(screen.getByText("Line 3: Missing participant_id")).toBeDefined();
    expect(screen.getByText("1 of 1 selected")).toBeDefined();
  });

  // A save that fails must leave the dialog open with the reason on screen,
  // rather than closing as though the peers had been written.
  it("keeps the dialog open and shows the error when the save fails", async () => {
    const onSave = vi.fn().mockRejectedValue(new Error("peers table is locked"));
    const onClose = vi.fn();
    const charlie = peer({
      participant_id: "c::1220cccc",
      name: "Charlie",
      address: "charlie.example.com",
      port: 9002,
      public_key: "keyC",
    });

    renderDialog({ mode: "import", onSave, onClose });
    uploadCsv(peersToCsv([charlie]));

    await waitFor(() => expect(screen.getByText("1 of 1 selected")).toBeDefined());
    fireEvent.click(screen.getByRole("button", { name: /Import 1/ }));

    await waitFor(() =>
      expect(screen.getByText("peers table is locked")).toBeDefined(),
    );
    expect(onClose).not.toHaveBeenCalled();
  });
});
