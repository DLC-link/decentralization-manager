import { TextField } from "@mui/material";
import { fieldHelpAdornment } from "../FieldHelp";

interface TextValueInputProps {
  value: string;
  onChange: (value: string) => void;
  help: string;
  /** The field name in the help icon's aria label, "Help for <name>". */
  helpName: string;
}

/** The input of a text field. */
export const TextValueInput = ({
  value,
  onChange,
  help,
  helpName,
}: TextValueInputProps) => (
  <TextField
    size="small"
    label="Value"
    value={value}
    onChange={(e) => onChange(e.target.value)}
    sx={{ flex: 1 }}
    slotProps={{
      input: {
        endAdornment: fieldHelpAdornment(help, `Help for ${helpName}`),
      },
    }}
  />
);
