import { Box, Typography } from "@mui/material";
import { legendSx } from "../../styles";
import { CopyableText } from "../CopyableText";

/** A labelled list of contract or participant ids, or nothing when empty. */
const IdList = ({ label, ids }: { label: string; ids: string[] }) =>
  ids.length === 0 ? null : (
    <Box sx={{ display: "flex", flexDirection: "column", gap: 0.5 }}>
      <Typography component="span" sx={{ ...legendSx, fontSize: "0.65rem" }}>
        {label}
      </Typography>
      {ids.map((id) => (
        <CopyableText key={id} text={id} truncate={{ start: 24, end: 12 }} variant="body2" />
      ))}
    </Box>
  );

export { IdList };
