import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { CredentialOfferInfo } from "../../../types";
import { CredentialOfferPicker } from "./CredentialOfferPicker";

const offer: CredentialOfferInfo = {
  contract_id: "00offer000000000012345678",
  operator: "op::1220",
  issuer: "issuer::1220",
  holder: "gov::1220",
  credential_id: "kyc-basic",
  description: "Basic KYC",
  is_free: true,
};

// The picker is the only combobox on the page.
const field = () => screen.getByRole("combobox");

describe("CredentialOfferPicker", () => {
  it("lists each offer by credential, issuer and cid tail, and picks its cid", () => {
    const setValue = vi.fn();
    render(
      <CredentialOfferPicker value="" setValue={setValue} offers={[offer]} kind="free" loading={false} />,
    );

    expect(screen.getByText("Pick a pending offer, or paste a contract id")).toBeTruthy();
    fireEvent.mouseDown(field());
    const option = screen.getByRole("option", { name: /kyc-basic — issuer \(…12345678\)/ });
    expect(option.textContent).toContain("Basic KYC");
    fireEvent.click(option);

    expect(setValue).toHaveBeenCalledWith(offer.contract_id);
  });

  it("keeps a pasted contract id", () => {
    const setValue = vi.fn();
    render(<CredentialOfferPicker value="" setValue={setValue} offers={[]} kind="paid" loading={false} />);

    fireEvent.change(field(), { target: { value: "00pasted" } });

    expect(setValue).toHaveBeenCalledWith("00pasted");
  });

  it("names the offer kind when none are visible", () => {
    render(<CredentialOfferPicker value="" setValue={() => {}} offers={[]} kind="paid" loading={false} />);

    expect(
      screen.getByText(
        "No pending paid offers visible — paste a contract id directly if you have one",
      ),
    ).toBeTruthy();
  });

  it("says when the offers are still loading", () => {
    render(<CredentialOfferPicker value="" setValue={() => {}} offers={[]} kind="free" loading />);

    expect(screen.getByText("Loading pending offers…")).toBeTruthy();
  });
});
