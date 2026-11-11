import type { ProposalState } from "./useProposalState";
import type { ProposalType } from "../../../types";

// The proposal payloads of the Transfer and Accept Transfer forms.

/// Default validity window (hours) for a Transfer proposal / two-step offer.
/// Mirrors the backend default; overridable per-transfer in the form.
const DEFAULT_TRANSFER_EXPIRY_HOURS = 24;

const buildTransfer = ({
  proposalTransferFactoryCid,
  proposalExpectedAdmin,
  proposalReceiver,
  proposalAmount,
  proposalInstrumentIdAdmin,
  proposalInstrumentIdId,
  proposalInputHoldingCids,
  proposalTransferExpiryHours,
}: Pick<
  ProposalState,
  | "proposalTransferFactoryCid"
  | "proposalExpectedAdmin"
  | "proposalReceiver"
  | "proposalAmount"
  | "proposalInstrumentIdAdmin"
  | "proposalInstrumentIdId"
  | "proposalInputHoldingCids"
  | "proposalTransferExpiryHours"
>): ProposalType => {
  // Send the override only when it's a valid positive integer that
  // differs from the default; otherwise omit it and let the backend
  // apply its default window.
  const expiryHours = Number(proposalTransferExpiryHours);
  const validityWindowHours =
    Number.isInteger(expiryHours) &&
    expiryHours > 0 &&
    expiryHours !== DEFAULT_TRANSFER_EXPIRY_HOURS
      ? expiryHours
      : undefined;
  return {
    type: "transfer",
    transfer_factory_cid: proposalTransferFactoryCid,
    expected_admin: proposalExpectedAdmin,
    receiver: proposalReceiver,
    amount: proposalAmount,
    instrument_id: { admin: proposalInstrumentIdAdmin, id: proposalInstrumentIdId },
    input_holding_cids: proposalInputHoldingCids ? proposalInputHoldingCids.split(",").map((s) => s.trim()).filter(Boolean) : [],
    ...(validityWindowHours !== undefined && {
      validity_window_hours: validityWindowHours,
    }),
  };
};

const buildAcceptTransfer = ({
  proposalTransferInstructionCid,
}: Pick<ProposalState, "proposalTransferInstructionCid">): ProposalType => {
  return {
    type: "accept_transfer",
    transfer_instruction_cid: proposalTransferInstructionCid,
  };
};

export {
  DEFAULT_TRANSFER_EXPIRY_HOURS,
  buildTransfer,
  buildAcceptTransfer,
};
