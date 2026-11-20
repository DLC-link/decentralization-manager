import { useState } from "react";
import { TableCell, Tooltip } from "@mui/material";

// A version stays on one line; one longer than its column ends in an ellipsis
// and shows in full on hover instead of running into the next column.
const versionCellSx = {
  py: 1,
  whiteSpace: "nowrap",
  overflow: "hidden",
  textOverflow: "ellipsis",
} as const;

// The full version shows on hover only when the column cut it off.
const VersionCell = ({ version }: { version?: string }) => {
  const [cutOff, setCutOff] = useState(false);
  return (
    <Tooltip title={cutOff && version ? version : ""} arrow>
      <TableCell
        sx={versionCellSx}
        onMouseEnter={(e) =>
          setCutOff(e.currentTarget.scrollWidth > e.currentTarget.clientWidth)
        }
      >
        {version || "-"}
      </TableCell>
    </Tooltip>
  );
};

export { VersionCell };
