import { Box, Button, Typography } from "@mui/material";
import { TextHelp } from "../../FieldHelp";
import { HelpTextField } from "./HelpTextField";

/** One requirement-editor row. `claims` holds a prefilled row's exact claims. */
export type RequirementRow = {
  issuer: string;
  claimsText: string;
  claims?: { property: string; value: string }[];
};

interface RequirementRowsProps {
  label: string;
  help: string;
  rows: RequirementRow[];
  setRows: (rows: RequirementRow[]) => void;
  /** The issuer of a newly added row. */
  defaultIssuer: string;
}

// Requirement-list editor shared by the Create Provider Configuration and
// Provision Instrument forms. Each row is an issuer party id plus the
// claims a credential from that issuer must carry, one "property,value"
// per line. Mirrors the beneficiaries row editor.
export const RequirementRows = ({
  label,
  help,
  rows,
  setRows,
  defaultIssuer,
}: RequirementRowsProps) => (
  <>
    <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
      <TextHelp text={help}>{label} (add issuer + required claims)</TextHelp>
    </Typography>
    {rows.map((row, idx) => (
      <Box key={idx} sx={{ display: "flex", gap: 1, mb: 1, alignItems: "flex-start" }}>
        <HelpTextField
          label="Issuer Party"
          value={row.issuer}
          onChange={(e) => {
            const updated = [...rows];
            updated[idx] = { ...row, issuer: e.target.value };
            setRows(updated);
          }}
          size="small"
          sx={{ flex: 2 }}
          help="Party id whose credential satisfies this requirement. The governance party mints the credential itself when it is the issuer; other issuers must credential the party out-of-band."
        />
        <HelpTextField
          label="Required claims (one per line: property,value)"
          value={row.claimsText}
          onChange={(e) => {
            const updated = [...rows];
            updated[idx] = { ...row, claimsText: e.target.value };
            setRows(updated);
          }}
          size="small"
          sx={{ flex: 3 }}
          multiline
          minRows={1}
          maxRows={4}
          help={'Claims the issuer\'s credential must contain. One per line, each formatted as "property,value" (e.g. role,registrar).'}
          helpLabel="Required Claims"
        />
        <Button
          size="small"
          color="error"
          onClick={() => setRows(rows.filter((_, i) => i !== idx))}
        >
          Remove
        </Button>
      </Box>
    ))}
    <Box sx={{ display: "flex", alignItems: "center", gap: 2 }}>
      <Button
        size="small"
        onClick={() => setRows([...rows, { issuer: defaultIssuer, claimsText: "" }])}
      >
        Add Requirement
      </Button>
    </Box>
  </>
);
