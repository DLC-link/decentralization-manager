import { describe, expect, it } from "vitest";

import {
  DEFAULT_TRANSFER_EXPIRY_HOURS,
  buildAcceptTransfer,
  buildTransfer,
} from "./transferPayloads";

const fields = {
  proposalTransferFactoryCid: "factory-1",
  proposalExpectedAdmin: "admin::1",
  proposalReceiver: "bob::1",
  proposalAmount: "2.5",
  proposalInstrumentIdAdmin: "admin::1",
  proposalInstrumentIdId: "TOK",
  proposalInputHoldingCids: "",
  proposalTransferExpiryHours: String(DEFAULT_TRANSFER_EXPIRY_HOURS),
};

describe("buildTransfer", () => {
  it("leaves the window to the backend at the default expiry", () => {
    expect(buildTransfer(fields)).toEqual({
      type: "transfer",
      transfer_factory_cid: "factory-1",
      expected_admin: "admin::1",
      receiver: "bob::1",
      amount: "2.5",
      instrument_id: { admin: "admin::1", id: "TOK" },
      input_holding_cids: [],
    });
  });

  it("sends a whole-hour expiry that differs from the default", () => {
    expect(buildTransfer({ ...fields, proposalTransferExpiryHours: "48" })).toMatchObject({
      validity_window_hours: 48,
    });
  });

  it("drops an expiry that is not a positive whole number", () => {
    for (const hours of ["1.5", "0", "-3", "", "abc"]) {
      expect(buildTransfer({ ...fields, proposalTransferExpiryHours: hours })).not.toHaveProperty(
        "validity_window_hours",
      );
    }
  });

  it("splits, trims and filters the pinned holding cids", () => {
    expect(buildTransfer({ ...fields, proposalInputHoldingCids: " h1 ,, h2 ," })).toMatchObject({
      input_holding_cids: ["h1", "h2"],
    });
  });
});

describe("buildAcceptTransfer", () => {
  it("sends the transfer instruction cid", () => {
    expect(buildAcceptTransfer({ proposalTransferInstructionCid: "ti-1" })).toEqual({
      type: "accept_transfer",
      transfer_instruction_cid: "ti-1",
    });
  });
});
