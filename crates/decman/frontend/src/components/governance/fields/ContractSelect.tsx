import type { ReactNode } from "react";
import { FormControl, InputLabel, MenuItem, Select } from "@mui/material";
import { TextHelp } from "../../FieldHelp";

interface ContractSelectProps<T extends { contract_id: string }> {
  label: string;
  /** The tooltip on the label. */
  help: string;
  value: string;
  onChange: (contractId: string) => void;
  items: T[];
  /** While true, the menu holds only the `loadingText` row. */
  loading?: boolean;
  loadingText?: string;
  /** The row the menu shows when `items` is empty. */
  emptyText: string;
  /** The text of an item's row. Defaults to its contract id. */
  rowLabel?: (item: T) => ReactNode;
  disabled?: boolean;
}

/** A required picker of contracts, one row per item, keyed by contract id. */
export const ContractSelect = <T extends { contract_id: string }>({
  label,
  help,
  value,
  onChange,
  items,
  loading,
  loadingText,
  emptyText,
  rowLabel = (item) => item.contract_id,
  disabled,
}: ContractSelectProps<T>) => (
  <FormControl size="small" fullWidth required disabled={disabled}>
    <InputLabel>
      <TextHelp text={help}>{label}</TextHelp>
    </InputLabel>
    <Select
      label={label}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      MenuProps={{ disableScrollLock: true }}
    >
      {loading ? (
        <MenuItem disabled>{loadingText}</MenuItem>
      ) : items.length > 0 ? (
        items.map((item) => (
          <MenuItem key={item.contract_id} value={item.contract_id}>
            {rowLabel(item)}
          </MenuItem>
        ))
      ) : (
        <MenuItem disabled>{emptyText}</MenuItem>
      )}
    </Select>
  </FormControl>
);
