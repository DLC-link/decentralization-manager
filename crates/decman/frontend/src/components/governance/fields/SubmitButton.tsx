import { Box, Button, CircularProgress, Portal } from "@mui/material";
import type { SxProps, Theme } from "@mui/material";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";

interface SubmitButtonProps {
  label: string;
  /** Shows a spinner in place of the icon. */
  loading: boolean;
  disabled: boolean;
  onClick: () => void;
  /**
   * When set, a plain button renders into this node, for a dialog footer.
   * Otherwise a contained button renders in place.
   */
  portalEl?: HTMLElement | null;
  /** When set, the in-place button sits in a Box with this sx. */
  inlineBoxSx?: SxProps<Theme>;
}

/** The submit button of the action and proposal forms. */
export const SubmitButton = ({
  label,
  loading,
  disabled,
  onClick,
  portalEl,
  inlineBoxSx,
}: SubmitButtonProps) => {
  if (portalEl) {
    return (
      <Portal container={portalEl}>
        <Button
          onClick={onClick}
          disabled={disabled}
          startIcon={loading ? <CircularProgress size={16} /> : undefined}
        >
          {label}
        </Button>
      </Portal>
    );
  }
  const button = (
    <Button
      variant="contained"
      onClick={onClick}
      disabled={disabled}
      startIcon={loading ? <CircularProgress size={16} /> : <CheckCircleIcon />}
    >
      {label}
    </Button>
  );
  return inlineBoxSx ? <Box sx={inlineBoxSx}>{button}</Box> : button;
};
