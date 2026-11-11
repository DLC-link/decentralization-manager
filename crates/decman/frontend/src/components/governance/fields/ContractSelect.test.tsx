import { fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ContractSelect } from "./ContractSelect";

const items = [
  { contract_id: "00aaaaaaaaaa", name: "Alpha" },
  { contract_id: "00bbbbbbbbbb", name: "Beta" },
];

const openMenu = () => {
  fireEvent.mouseDown(screen.getByRole("combobox"));
  return within(screen.getByRole("listbox"));
};

describe("ContractSelect", () => {
  it("lists one row per item, labelled by contract id", () => {
    const onChange = vi.fn();
    render(
      <ContractSelect
        label="RegistrarService"
        help="Which service."
        value=""
        onChange={onChange}
        items={items}
        emptyText="None found"
      />,
    );

    const menu = openMenu();
    expect(menu.getAllByRole("option").map((o) => o.textContent)).toEqual([
      "00aaaaaaaaaa",
      "00bbbbbbbbbb",
    ]);
    fireEvent.click(menu.getByRole("option", { name: "00bbbbbbbbbb" }));
    expect(onChange).toHaveBeenCalledWith("00bbbbbbbbbb");
  });

  it("uses rowLabel for the row text", () => {
    render(
      <ContractSelect
        label="Instrument"
        help="Which instrument."
        value=""
        onChange={() => {}}
        items={items}
        emptyText="None found"
        rowLabel={(item) => <>{item.name} ({item.contract_id.slice(0, 4)}…)</>}
      />,
    );

    expect(openMenu().getAllByRole("option").map((o) => o.textContent)).toEqual([
      "Alpha (00aa…)",
      "Beta (00bb…)",
    ]);
  });

  it("shows only the loading row while loading", () => {
    render(
      <ContractSelect
        label="RegistrarService"
        help="Which service."
        value=""
        onChange={() => {}}
        items={items}
        loading
        loadingText="Loading services…"
        emptyText="None found"
      />,
    );

    const rows = openMenu().getAllByRole("option");
    expect(rows.map((o) => o.textContent)).toEqual(["Loading services…"]);
    expect(rows[0].getAttribute("aria-disabled")).toBe("true");
  });

  it("shows the empty row when there are no items", () => {
    render(
      <ContractSelect
        label="RegistrarService"
        help="Which service."
        value=""
        onChange={() => {}}
        items={[]}
        loading={false}
        loadingText="Loading services…"
        emptyText="None found"
      />,
    );

    const rows = openMenu().getAllByRole("option");
    expect(rows.map((o) => o.textContent)).toEqual(["None found"]);
    expect(rows[0].getAttribute("aria-disabled")).toBe("true");
  });

  it("marks the field required and can be disabled", () => {
    const { container } = render(
      <ContractSelect
        label="Instrument"
        help="Which instrument."
        value=""
        onChange={() => {}}
        items={items}
        emptyText="None found"
        disabled
      />,
    );

    const box = screen.getByRole("combobox");
    expect(box.getAttribute("aria-disabled")).toBe("true");
    expect(container.querySelector("input")?.required).toBe(true);
  });
});
