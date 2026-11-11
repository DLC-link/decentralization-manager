import { Autocomplete, Button, Typography, Box, TextField, Accordion, AccordionSummary, AccordionDetails, IconButton, Divider } from "@mui/material";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import AddIcon from "@mui/icons-material/Add";
import DeleteIcon from "@mui/icons-material/Delete";
import { TextHelp, fieldHelpAdornment } from "../FieldHelp";
import type { ContractDefinition, FieldDefinition } from "../../types";
import { FieldEditor } from "./FieldEditor";

interface ContractEditorProps {
  contract: ContractDefinition;
  onChange: (contract: ContractDefinition) => void;
  onDelete: () => void;
  index: number;
  participantCount: number;
  partyId: string;
  knownPackageIds: string[];
  lockStructure?: boolean;
}

const ContractEditor = ({
  contract,
  onChange,
  onDelete,
  index,
  participantCount,
  partyId,
  knownPackageIds,
  lockStructure = false,
}: ContractEditorProps) => {
  const handleFieldChange = (fieldIndex: number, newField: FieldDefinition) => {
    const newFields = [...contract.fields];
    newFields[fieldIndex] = newField;
    onChange({ ...contract, fields: newFields });
  };

  const handleAddField = () => {
    onChange({
      ...contract,
      fields: [...contract.fields, { type: "text", value: "" }],
    });
  };

  const handleDeleteField = (fieldIndex: number) => {
    onChange({
      ...contract,
      fields: contract.fields.filter((_, i) => i !== fieldIndex),
    });
  };

  return (
    <Accordion
      defaultExpanded={index === 0}
      sx={{
        borderRadius: 3,
        mb: 1,
        "&:first-of-type": { borderRadius: 3 },
        "&:last-of-type": { borderRadius: 3 },
        overflow: "hidden",
      }}
    >
      <AccordionSummary
        expandIcon={<ExpandMoreIcon />}
        sx={{ borderRadius: "12px 12px 0 0" }}
      >
        <Box
          sx={{
            display: "flex",
            alignItems: "center",
            width: "100%",
            justifyContent: "space-between",
          }}
        >
          <Typography>{contract.name || `Contract ${index + 1}`}</Typography>
          {!lockStructure && (
            <IconButton
              size="small"
              onClick={(e) => {
                e.stopPropagation();
                onDelete();
              }}
              color="error"
            >
              <DeleteIcon fontSize="small" />
            </IconButton>
          )}
        </Box>
      </AccordionSummary>
      <AccordionDetails sx={{ p: 3 }}>
        <Box sx={{ display: "flex", flexDirection: "column", gap: 2 }}>
          <Autocomplete
            freeSolo
            options={knownPackageIds}
            value={contract.package_id}
            onChange={(_e, value) =>
              onChange({ ...contract, package_id: value || "" })
            }
            onInputChange={(_e, value) =>
              onChange({ ...contract, package_id: value })
            }
            size="small"
            sx={{ flex: 1 }}
            renderInput={(params) => (
              <TextField
                {...params}
                label={
                  <TextHelp text="Canton package that contains the contract template. Pick an uploaded DAR from the list, or paste a full package ID hash.">
                    Package Name / ID
                  </TextHelp>
                }
                placeholder="Enter or select package ID"
              />
            )}
          />
          <Box sx={{ display: "flex", gap: 2 }}>
            <TextField
              size="small"
              label="Module Name"
              value={contract.module_name}
              onChange={(e) =>
                onChange({ ...contract, module_name: e.target.value })
              }
              fullWidth
              placeholder="e.g., CBTC.Governance"
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Dotted Daml module path inside the package (e.g. CBTC.Governance). Must match the module where the contract template is defined.",
                    "Help for Module Name",
                  ),
                },
              }}
            />
            <TextField
              size="small"
              label="Entity Name"
              value={contract.entity_name}
              onChange={(e) =>
                onChange({ ...contract, entity_name: e.target.value })
              }
              fullWidth
              placeholder="e.g., CBTCGovernanceRules"
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Name of the contract template inside the module (e.g. CBTCGovernanceRules). This is the type whose record fields you fill in below.",
                    "Help for Entity Name",
                  ),
                },
              }}
            />
          </Box>

          <Divider />

          <Typography variant="subtitle2">Record Fields</Typography>
          {contract.fields.map((field, fieldIndex) => (
            <FieldEditor
              key={fieldIndex}
              field={field}
              onChange={(newField) => handleFieldChange(fieldIndex, newField)}
              onDelete={() => handleDeleteField(fieldIndex)}
              participantCount={participantCount}
              partyId={partyId}
              lockStructure={lockStructure}
              label={contract.fieldLabels?.[fieldIndex]}
            />
          ))}
          {!lockStructure && (
            <Button
              startIcon={<AddIcon />}
              onClick={handleAddField}
              variant="outlined"
              size="small"
            >
              Add Field
            </Button>
          )}
        </Box>
      </AccordionDetails>
    </Accordion>
  );
};

export { ContractEditor };
