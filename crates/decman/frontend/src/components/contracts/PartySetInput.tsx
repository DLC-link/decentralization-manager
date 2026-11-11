import { Box, IconButton, TextField, Typography } from "@mui/material";
import DeleteIcon from "@mui/icons-material/Delete";
import { fieldHelpAdornment } from "../FieldHelp";
import { commitPartyInput } from "./contractFields";

interface PartySetInputProps {
  parties: string[];
  onChange: (parties: string[]) => void;
  placeholder: string;
  help: string;
  /** The field name in the help icon's aria label, "Help for <name>". */
  helpName: string;
}

/**
 * The editor of a party_set field: an input that adds a pasted party id,
 * and one removable row per party.
 */
export const PartySetInput = ({
  parties,
  onChange,
  placeholder,
  help,
  helpName,
}: PartySetInputProps) => (
  <Box sx={{ display: "flex", flexDirection: "column", gap: 1, flex: 1 }}>
    <TextField
      size="small"
      placeholder={placeholder}
      fullWidth
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          commitPartyInput(
            e.currentTarget as HTMLInputElement,
            parties,
            onChange,
          );
          e.preventDefault();
        }
      }}
      onBlur={(e) => commitPartyInput(e.target, parties, onChange)}
      slotProps={{
        input: {
          endAdornment: fieldHelpAdornment(help, `Help for ${helpName}`),
        },
      }}
    />
    {parties.length > 0 && (
      <Box sx={{ display: "flex", flexDirection: "column", gap: 0.5 }}>
        {parties.map((party, idx) => (
          <Box
            key={idx}
            sx={{
              display: "flex",
              alignItems: "center",
              bgcolor: "action.hover",
              borderRadius: 1,
              px: 1,
              py: 0.25,
            }}
          >
            <Typography
              variant="caption"
              sx={{ flex: 1, fontFamily: "var(--font-mono)" }}
            >
              {party}
            </Typography>
            <IconButton
              size="small"
              onClick={() => onChange(parties.filter((_, i) => i !== idx))}
              sx={{ p: 0.25 }}
            >
              <DeleteIcon sx={{ fontSize: 14 }} />
            </IconButton>
          </Box>
        ))}
      </Box>
    )}
  </Box>
);
