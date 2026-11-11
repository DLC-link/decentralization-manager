import { TextField, type TextFieldProps } from "@mui/material";
import { fieldHelpAdornment } from "../../FieldHelp";

type HelpTextFieldProps = TextFieldProps & {
  label: string;
  /** The text of the help icon's tooltip. */
  help: string;
  /**
   * The field name in the help icon's aria label, "Help for <name>".
   * Defaults to `label`.
   */
  helpLabel?: string;
};

/**
 * A TextField with a help icon at its end. The icon takes `slotProps.input`,
 * so pass the other slots only.
 */
export const HelpTextField = ({
  help,
  helpLabel,
  slotProps,
  ...props
}: HelpTextFieldProps) => (
  <TextField
    {...props}
    slotProps={{
      ...slotProps,
      input: {
        endAdornment: fieldHelpAdornment(
          help,
          `Help for ${helpLabel ?? props.label}`,
        ),
      },
    }}
  />
);
