import { Chip, Tooltip } from "@mui/material";

interface SharedPartiesChipProps {
  /// Parties that this node and the participant both host.
  count: number;
  /// The participant's display name, for the tooltip.
  name: string;
  onClick: () => void;
}

/** Opens the party list filtered to the parties this node shares with a participant. */
export const SharedPartiesChip = ({ count, name, onClick }: SharedPartiesChipProps) => (
  <Tooltip title={`Show the parties this node shares with ${name}`} describeChild>
    <span>
      <Chip
        size="small"
        variant="outlined"
        label={`${count} shared`}
        onClick={onClick}
        disabled={count === 0}
        sx={{ height: 20, fontSize: "0.7rem" }}
      />
    </span>
  </Tooltip>
);
