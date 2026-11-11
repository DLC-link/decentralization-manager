import { TextField } from "@mui/material";
import { fieldHelpAdornment } from "../FieldHelp";

interface IntegerInputProps {
  value: number;
  /** Gets 0 when the input is not a number. */
  onChange: (value: number) => void;
  /** The input width, in pixels. */
  width: number;
  help: string;
  /** The field name in the help icon's aria label, "Help for <name>". */
  helpName: string;
}

/** The number input of an int64 field. */
export const IntegerInput = ({
  value,
  onChange,
  width,
  help,
  helpName,
}: IntegerInputProps) => (
  <TextField
    size="small"
    label="Value"
    type="number"
    value={value}
    onChange={(e) => onChange(parseInt(e.target.value) || 0)}
    sx={{ width }}
    slotProps={{
      input: {
        endAdornment: fieldHelpAdornment(help, `Help for ${helpName}`),
      },
    }}
  />
);
