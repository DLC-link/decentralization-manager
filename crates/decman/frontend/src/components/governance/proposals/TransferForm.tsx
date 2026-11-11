import {
  Autocomplete,
  Box,
  Button,
  Chip,
  MenuItem,
  TextField,
  Typography,
} from "@mui/material";
import { TextHelp } from "../../FieldHelp";
import { HelpTextField } from "../fields/HelpTextField";
import { DEFAULT_TRANSFER_EXPIRY_HOURS } from "./transferPayloads";
import type { ProposalState } from "./useProposalState";
import type { Holding } from "../../../types";

/// Freely-transferable balance of a holding: total minus the locked
/// (escrowed) portion. A transfer can only be funded from unlocked holdings,
/// so the amount field is capped on this rather than `amount`.
const holdingAvailable = (h: Holding): number =>
  Number(h.amount) - Number(h.locked_amount);

interface Props {
  state: ProposalState;
}

/** The Transfer and Accept Transfer forms. */
export const TransferForm = ({ state }: Props) => {
  const {
    proposalType,
    proposalReceiver,
    setProposalReceiver,
    proposalAmount,
    setProposalAmount,
    proposalInputHoldingCids,
    setProposalInputHoldingCids,
    proposalTransferExpiryHours,
    setProposalTransferExpiryHours,
    proposalTransferInstructionCid,
    setProposalTransferInstructionCid,
    openTransferInstructions,
    transferInstructionsLoading,
    transferHoldings,
    transferFactories,
    transferPrefillLoading,
    selectedHoldingKey,
    setSelectedHoldingKey,
    showTransferAdvanced,
    setShowTransferAdvanced,
  } = state;

  return (
    <>
      {proposalType === "transfer" && (
        <>
          <HelpTextField
            select
            size="small"
            label="Instrument"
            value={selectedHoldingKey}
            onChange={(e) => setSelectedHoldingKey(e.target.value)}
            fullWidth
            required
            disabled={transferPrefillLoading}
            helperText={
              transferPrefillLoading
                ? "Loading holdings…"
                : transferHoldings.length === 0
                  ? "No holdings available for this party"
                  : "Pick an instrument — admin, ID, factory CID and expected admin will be prefilled"
            }
            help="Token to transfer, picked from this party's holdings. Selecting one prefills the matching TransferFactory and expected admin."
          >
            {transferHoldings.map((h) => {
              const key = `${h.instrument_admin}::${h.instrument_id}`;
              const hasFactory = transferFactories.some(
                (f) => f.expected_admin === h.instrument_admin,
              );
              // Canton Coin's token-standard instrument_id is the
              // literal "Amulet" — display it as "CC" to match the
              // Holdings section.
              const label =
                h.instrument_id === "Amulet" ? "CC" : h.instrument_id;
              return (
                <MenuItem
                  key={key}
                  value={key}
                  disabled={!hasFactory}
                >
                  {label} — available {holdingAvailable(h)}
                  {Number(h.locked_amount) > 0 &&
                    ` (${h.locked_amount} locked)`}
                  {!hasFactory && " (no factory available)"}
                </MenuItem>
              );
            })}
          </HelpTextField>
          {selectedHoldingKey &&
            (() => {
              const holding = transferHoldings.find(
                (h) =>
                  `${h.instrument_admin}::${h.instrument_id}` ===
                  selectedHoldingKey,
              );
              return holding ? (
                <Box
                  sx={{
                    display: "flex",
                    gap: 1,
                    flexWrap: "wrap",
                    alignItems: "center",
                  }}
                >
                  <Chip
                    size="small"
                    label={`Available balance: ${holdingAvailable(holding)}`}
                    color="primary"
                  />
                  {Number(holding.locked_amount) > 0 && (
                    <Chip
                      size="small"
                      label={`Locked: ${holding.locked_amount}`}
                      variant="outlined"
                      color="warning"
                    />
                  )}
                  <Chip
                    size="small"
                    label={`Admin: ${holding.instrument_admin}`}
                    variant="outlined"
                  />
                </Box>
              ) : null;
            })()}
          <HelpTextField
            size="small"
            label="Receiver Party"
            value={proposalReceiver}
            onChange={(e) => setProposalReceiver(e.target.value)}
            fullWidth
            required
            help="Party id that will receive the transferred tokens."
          />
          <HelpTextField
            size="small"
            label="Amount"
            value={proposalAmount}
            onChange={(e) => setProposalAmount(e.target.value)}
            fullWidth
            required
            type="number"
            slotProps={{
              htmlInput: { min: 0, step: "any" },
            }}
            help="How much of the selected instrument to transfer. Must be positive and not exceed the available balance."
            error={(() => {
              if (!proposalAmount) return false;
              const n = Number(proposalAmount);
              if (!Number.isFinite(n) || n <= 0) return true;
              const holding = transferHoldings.find(
                (h) =>
                  `${h.instrument_admin}::${h.instrument_id}` ===
                  selectedHoldingKey,
              );
              return holding ? n > holdingAvailable(holding) : false;
            })()}
            helperText={(() => {
              if (!proposalAmount) return "";
              const n = Number(proposalAmount);
              if (!Number.isFinite(n) || n <= 0)
                return "Enter a positive amount";
              const holding = transferHoldings.find(
                (h) =>
                  `${h.instrument_admin}::${h.instrument_id}` ===
                  selectedHoldingKey,
              );
              if (holding && n > holdingAvailable(holding)) {
                return `Exceeds available balance (${holdingAvailable(holding)})`;
              }
              return "";
            })()}
          />
          <HelpTextField
            size="small"
            label="Offer expiry (hours)"
            value={proposalTransferExpiryHours}
            onChange={(e) =>
              setProposalTransferExpiryHours(e.target.value)
            }
            fullWidth
            type="number"
            slotProps={{
              htmlInput: { min: 1, step: 1 },
            }}
            help={`How long the transfer stays valid (default ${DEFAULT_TRANSFER_EXPIRY_HOURS}h). If the recipient isn't preapproved, this becomes an offer they must accept before it expires; after expiry the escrowed funds are released back to you.`}
            helpLabel="Offer expiry"
            error={(() => {
              const n = Number(proposalTransferExpiryHours);
              return (
                proposalTransferExpiryHours !== "" &&
                (!Number.isInteger(n) || n <= 0)
              );
            })()}
            helperText={(() => {
              const n = Number(proposalTransferExpiryHours);
              if (
                proposalTransferExpiryHours !== "" &&
                (!Number.isInteger(n) || n <= 0)
              ) {
                return "Enter a positive whole number of hours";
              }
              return "";
            })()}
          />
          <Button
            size="small"
            variant="text"
            onClick={() => setShowTransferAdvanced((v) => !v)}
            sx={{ alignSelf: "flex-start", textTransform: "none" }}
          >
            {showTransferAdvanced ? "Hide advanced" : "Show advanced"}
          </Button>
          {showTransferAdvanced && (
            <HelpTextField
              size="small"
              label="Input Holding CIDs (comma-separated)"
              value={proposalInputHoldingCids}
              onChange={(e) =>
                setProposalInputHoldingCids(e.target.value)
              }
              fullWidth
              helperText="Optional — pin specific Holding contracts to spend. Leave empty to let the server select your holdings of the chosen instrument automatically (change is returned)."
              help="Optional list of specific Holding contract ids to spend, comma-separated. Leave empty to let the server select your holdings of the chosen instrument automatically; the transfer consumes what it needs and returns change."
              helpLabel="Input Holding CIDs"
            />
          )}
        </>
      )}

      {proposalType === "accept_transfer" && (
        <Autocomplete
          size="small"
          freeSolo
          options={openTransferInstructions}
          value={proposalTransferInstructionCid}
          loading={transferInstructionsLoading}
          onChange={(_event, value) => {
            if (typeof value === "string" || value === null) {
              setProposalTransferInstructionCid(value ?? "");
            } else {
              setProposalTransferInstructionCid(value.contract_id);
            }
          }}
          onInputChange={(_event, value, reason) => {
            // Keep the field in sync when the user types a cid by
            // hand (freeSolo fallback). `reset` fires when an option
            // is selected; we already handled that via `onChange`.
            if (reason === "input") {
              setProposalTransferInstructionCid(value);
            }
          }}
          getOptionLabel={(option) => {
            if (typeof option === "string") return option;
            // Strip the `::1220…` fingerprint suffix from the party
            // id so the label fits in the dropdown; show a short cid
            // tail for disambiguation when multiple transfers share
            // a sender/amount.
            const senderName = option.sender.split("::")[0];
            const amount = option.amount.replace(/\.?0+$/, "");
            const cidTail = option.contract_id.slice(-8);
            return `${senderName} → ${amount} ${option.instrument_id} (…${cidTail})`;
          }}
          getOptionDisabled={(option) => {
            if (typeof option === "string") return false;
            if (option.status === "pending_internal_workflow") return true;
            const exp = option.expires_at ?? 0;
            return exp > 0 && exp <= Math.floor(Date.now() / 1000);
          }}
          isOptionEqualToValue={(option, value) =>
            typeof value === "string"
              ? option.contract_id === value
              : option.contract_id === value.contract_id
          }
          renderOption={(props, option) => {
            if (typeof option === "string") {
              return <li {...props}>{option}</li>;
            }
            const senderName = option.sender.split("::")[0];
            const amount = option.amount.replace(/\.?0+$/, "");
            const cidTail = option.contract_id.slice(-8);
            const isBlocked =
              option.status === "pending_internal_workflow";
            const exp = option.expires_at ?? 0;
            const isExpired =
              exp > 0 && exp <= Math.floor(Date.now() / 1000);
            const pendingSummary = (option.pending_actions ?? [])
              .map((p) => {
                const partyName = p.party.split("::")[0];
                return p.action
                  ? `${partyName} — ${p.action}`
                  : partyName;
              })
              .join(", ");
            return (
              <li {...props} key={option.contract_id}>
                <Box
                  sx={{
                    display: "flex",
                    flexDirection: "column",
                    gap: 0.25,
                    opacity: isBlocked || isExpired ? 0.6 : 1,
                  }}
                >
                  <Typography variant="body2">
                    {senderName} → {amount} {option.instrument_id} (…
                    {cidTail})
                  </Typography>
                  {isExpired && (
                    <Typography variant="caption" color="warning.main">
                      Expired {new Date(exp * 1000).toLocaleString()}
                    </Typography>
                  )}
                  {!isExpired && isBlocked && (
                    <Typography variant="caption" color="warning.main">
                      Waiting on{pendingSummary ? `: ${pendingSummary}` : " internal workflow"}
                    </Typography>
                  )}
                </Box>
              </li>
            );
          }}
          renderInput={(params) => (
            <TextField
              {...params}
              label={
                <TextHelp text="Contract id of the pending TransferInstruction this party will accept.">
                  TransferInstruction Contract ID
                </TextHelp>
              }
              required
              helperText={
                transferInstructionsLoading
                  ? "Loading open transfers…"
                  : openTransferInstructions.length === 0
                    ? "No open transfers visible — paste a contract id directly if you have one"
                    : "Pick an open transfer, or paste a contract id"
              }
            />
          )}
        />
      )}
    </>
  );
};
