import { Autocomplete, Box, TextField, Typography } from "@mui/material";
import { offerBillingSummary } from "../../../paidCredential";
import type { CredentialOfferInfo } from "../../../types";
import { TextHelp } from "../../FieldHelp";

interface CredentialOfferPickerProps {
  value: string;
  setValue: (v: string) => void;
  offers: CredentialOfferInfo[];
  /** Names the offers in the empty-list hint. */
  kind: "free" | "paid";
  loading: boolean;
}

// CredentialOffer picker shared by the Accept Free and Accept Paid
// Credential proposal forms. freeSolo keeps hand-pasting a cid possible when
// the offer isn't visible to this participant.
export const CredentialOfferPicker = ({
  value,
  setValue,
  offers,
  kind,
  loading,
}: CredentialOfferPickerProps) => (
  <Autocomplete
    size="small"
    freeSolo
    options={offers}
    value={value}
    loading={loading}
    onChange={(_event, newValue) => {
      if (typeof newValue === "string" || newValue === null) {
        setValue(newValue ?? "");
      } else {
        setValue(newValue.contract_id);
      }
    }}
    onInputChange={(_event, newValue, reason) => {
      // Keep the field in sync when the user types a cid by hand
      // (freeSolo fallback). `reset` fires when an option is selected;
      // we already handled that via `onChange`.
      if (reason === "input") {
        setValue(newValue);
      }
    }}
    getOptionLabel={(option) =>
      typeof option === "string" ? option : option.contract_id
    }
    isOptionEqualToValue={(option, val) =>
      typeof val === "string"
        ? option.contract_id === val
        : option.contract_id === val.contract_id
    }
    renderOption={(props, option) => {
      if (typeof option === "string") {
        return <li {...props}>{option}</li>;
      }
      const issuerName = option.issuer.split("::")[0];
      const cidTail = option.contract_id.slice(-8);
      const billing = offerBillingSummary(option);
      return (
        <li {...props} key={option.contract_id}>
          <Box sx={{ display: "flex", flexDirection: "column", gap: 0.25 }}>
            <Typography variant="body2">
              {option.credential_id} — {issuerName} (…{cidTail})
            </Typography>
            {option.description && (
              <Typography variant="caption" color="text.secondary">
                {option.description}
              </Typography>
            )}
            {billing && (
              <Typography variant="caption" color="text.secondary">
                {billing}
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
          <TextHelp text="Contract id of the pending CredentialOffer to accept.">
            CredentialOffer Contract ID
          </TextHelp>
        }
        required
        helperText={
          loading
            ? "Loading pending offers…"
            : offers.length === 0
              ? `No pending ${kind} offers visible — paste a contract id directly if you have one`
              : "Pick a pending offer, or paste a contract id"
        }
      />
    )}
  />
);
