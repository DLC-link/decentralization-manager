import { TextField } from "@mui/material";
import { fieldHelpAdornment } from "../FieldHelp";

interface ThresholdInputProps {
  /** The threshold. Unset shows `defaultThreshold`. */
  value: number | undefined;
  defaultThreshold: number;
  /** Gets `defaultThreshold` when the input is not a number. */
  onChange: (value: number) => void;
  help: string;
  /** The field name in the help icon's aria label, "Help for <name>". */
  helpName: string;
}

/** The number input of a governance_threshold field. */
export const ThresholdInput = ({
  value,
  defaultThreshold,
  onChange,
  help,
  helpName,
}: ThresholdInputProps) => (
  <TextField
    size="small"
    label="Value"
    type="number"
    value={value ?? defaultThreshold}
    onChange={(e) => onChange(parseInt(e.target.value) || defaultThreshold)}
    sx={{ width: 100 }}
    slotProps={{
      input: {
        endAdornment: fieldHelpAdornment(help, `Help for ${helpName}`),
      },
    }}
  />
);
