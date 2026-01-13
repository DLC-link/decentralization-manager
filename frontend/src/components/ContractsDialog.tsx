import { useState, useEffect, useCallback } from "react";
import {
  Dialog,
  DialogTitle,
  DialogContent,
  DialogActions,
  Button,
  Typography,
  CircularProgress,
  Alert,
  Box,
  TextField,
  Accordion,
  AccordionSummary,
  AccordionDetails,
  IconButton,
  Select,
  MenuItem,
  FormControl,
  InputLabel,
  Divider,
} from "@mui/material";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import AddIcon from "@mui/icons-material/Add";
import DeleteIcon from "@mui/icons-material/Delete";
import UploadFileIcon from "@mui/icons-material/UploadFile";
import { API_BASE } from "../constants";
import type {
  ContractsStatusResponse,
  ContractsRequest,
  ContractDefinition,
  FieldDefinition,
  DarFile,
} from "../types";

interface ContractsDialogProps {
  open: boolean;
  onClose: () => void;
  onComplete: () => void;
  partyId: string;
  participantIds: string[];
}

const FIELD_TYPES = [
  { value: "decentralized_party", label: "Decentralized Party" },
  { value: "operator_party", label: "Operator Party" },
  { value: "participant_party", label: "Participant Party" },
  { value: "text", label: "Text" },
  { value: "int64", label: "Integer (64-bit)" },
  { value: "bool", label: "Boolean" },
  { value: "instrument", label: "Instrument" },
  { value: "attestors_set", label: "Attestors Set" },
  { value: "governance_threshold", label: "Governance Threshold" },
  { value: "optional", label: "Optional" },
  { value: "record", label: "Record" },
];

const createEmptyContract = (): ContractDefinition => ({
  id: "",
  name: "",
  package_id: "",
  module_name: "",
  entity_name: "",
  fields: [],
});

// Default contracts from original contract-deploy.toml
const DEFAULT_OPERATOR_PARTY = "";

const getDefaultContracts = (): ContractDefinition[] => [
  {
    id: "create-govR",
    name: "CBTCGovernanceRules",
    package_id: "#cbtc-governance",
    module_name: "CBTC.Governance",
    entity_name: "CBTCGovernanceRules",
    fields: [
      { type: "decentralized_party" },
      { type: "operator_party" },
      { type: "instrument", id: "CBTC" },
      { type: "record", fields: [{ type: "attestors_set" }] },
      { type: "optional", inner: { type: "governance_threshold" } },
    ],
  },
  {
    id: "create-daR",
    name: "CBTCDepositAccountRules",
    package_id: "#cbtc",
    module_name: "CBTC.DepositAccount",
    entity_name: "CBTCDepositAccountRules",
    fields: [
      { type: "decentralized_party" },
      { type: "operator_party" },
      { type: "instrument", id: "CBTC" },
    ],
  },
  {
    id: "create-waR",
    name: "CBTCWithdrawAccountRules",
    package_id: "#cbtc",
    module_name: "CBTC.WithdrawAccount",
    entity_name: "CBTCWithdrawAccountRules",
    fields: [
      { type: "decentralized_party" },
      { type: "operator_party" },
      { type: "instrument", id: "CBTC" },
    ],
  },
];

const createDefaultField = (type: string): FieldDefinition => {
  switch (type) {
    case "decentralized_party":
      return { type: "decentralized_party" };
    case "operator_party":
      return { type: "operator_party" };
    case "participant_party":
      return { type: "participant_party", index: 0 };
    case "text":
      return { type: "text", value: "" };
    case "int64":
      return { type: "int64", value: 0 };
    case "bool":
      return { type: "bool", value: false };
    case "instrument":
      return { type: "instrument", id: "" };
    case "attestors_set":
      return { type: "attestors_set" };
    case "governance_threshold":
      return { type: "governance_threshold" };
    case "optional":
      return { type: "optional", inner: { type: "text", value: "" } };
    case "record":
      return { type: "record", fields: [] };
    default:
      return { type: "text", value: "" };
  }
};

interface FieldEditorProps {
  field: FieldDefinition;
  onChange: (field: FieldDefinition) => void;
  onDelete: () => void;
}

const FieldEditor = ({ field, onChange, onDelete }: FieldEditorProps) => {
  const handleTypeChange = (newType: string) => {
    onChange(createDefaultField(newType));
  };

  return (
    <Box
      sx={{
        display: "flex",
        gap: 1,
        alignItems: "flex-start",
        mb: 1,
        p: 1,
        border: "1px solid",
        borderColor: "divider",
        borderRadius: 1,
      }}
    >
      <FormControl size="small" sx={{ minWidth: 180 }}>
        <InputLabel>Field Type</InputLabel>
        <Select
          value={field.type}
          label="Field Type"
          onChange={(e) => handleTypeChange(e.target.value)}
        >
          {FIELD_TYPES.map((ft) => (
            <MenuItem key={ft.value} value={ft.value}>
              {ft.label}
            </MenuItem>
          ))}
        </Select>
      </FormControl>

      {field.type === "participant_party" && (
        <TextField
          size="small"
          label="Index"
          type="number"
          value={field.index}
          onChange={(e) =>
            onChange({ ...field, index: parseInt(e.target.value) || 0 })
          }
          sx={{ width: 100 }}
        />
      )}

      {field.type === "text" && (
        <TextField
          size="small"
          label="Value"
          value={field.value}
          onChange={(e) => onChange({ ...field, value: e.target.value })}
          sx={{ flex: 1 }}
        />
      )}

      {field.type === "int64" && (
        <TextField
          size="small"
          label="Value"
          type="number"
          value={field.value}
          onChange={(e) =>
            onChange({ ...field, value: parseInt(e.target.value) || 0 })
          }
          sx={{ width: 150 }}
        />
      )}

      {field.type === "bool" && (
        <FormControl size="small" sx={{ minWidth: 100 }}>
          <InputLabel>Value</InputLabel>
          <Select
            value={field.value ? "true" : "false"}
            label="Value"
            onChange={(e) =>
              onChange({ ...field, value: e.target.value === "true" })
            }
          >
            <MenuItem value="true">True</MenuItem>
            <MenuItem value="false">False</MenuItem>
          </Select>
        </FormControl>
      )}

      {field.type === "instrument" && (
        <TextField
          size="small"
          label="Instrument ID"
          value={field.id}
          onChange={(e) => onChange({ ...field, id: e.target.value })}
          sx={{ flex: 1 }}
        />
      )}

      {field.type === "optional" && (
        <Box
          sx={{
            flex: 1,
            pl: 2,
            borderLeft: "2px solid",
            borderColor: "primary.light",
          }}
        >
          <Typography
            variant="caption"
            color="text.secondary"
            sx={{ mb: 0.5, display: "block" }}
          >
            Inner type:
          </Typography>
          <FormControl size="small" sx={{ minWidth: 150 }}>
            <InputLabel>Inner Type</InputLabel>
            <Select
              value={field.inner?.type || "text"}
              label="Inner Type"
              onChange={(e) =>
                onChange({
                  ...field,
                  inner: createDefaultField(e.target.value),
                })
              }
            >
              {FIELD_TYPES.filter(
                (ft) => ft.value !== "optional" && ft.value !== "record",
              ).map((ft) => (
                <MenuItem key={ft.value} value={ft.value}>
                  {ft.label}
                </MenuItem>
              ))}
            </Select>
          </FormControl>
        </Box>
      )}

      {field.type === "record" && (
        <Box
          sx={{
            flex: 1,
            pl: 2,
            borderLeft: "2px solid",
            borderColor: "secondary.light",
          }}
        >
          <Typography
            variant="caption"
            color="text.secondary"
            sx={{ mb: 0.5, display: "block" }}
          >
            Record fields:
          </Typography>
          {field.fields?.map((subField, idx) => (
            <Box
              key={idx}
              sx={{ display: "flex", gap: 1, alignItems: "center", mb: 0.5 }}
            >
              <FormControl size="small" sx={{ minWidth: 150 }}>
                <Select
                  value={subField.type}
                  onChange={(e) => {
                    const newFields = [...(field.fields || [])];
                    newFields[idx] = createDefaultField(e.target.value);
                    onChange({ ...field, fields: newFields });
                  }}
                >
                  {FIELD_TYPES.filter((ft) => ft.value !== "record").map(
                    (ft) => (
                      <MenuItem key={ft.value} value={ft.value}>
                        {ft.label}
                      </MenuItem>
                    ),
                  )}
                </Select>
              </FormControl>
              <IconButton
                size="small"
                onClick={() => {
                  const newFields = (field.fields || []).filter(
                    (_, i) => i !== idx,
                  );
                  onChange({ ...field, fields: newFields });
                }}
              >
                <DeleteIcon fontSize="small" />
              </IconButton>
            </Box>
          ))}
          <Button
            size="small"
            startIcon={<AddIcon />}
            onClick={() =>
              onChange({
                ...field,
                fields: [...(field.fields || []), { type: "text", value: "" }],
              })
            }
          >
            Add Field
          </Button>
        </Box>
      )}

      <IconButton size="small" onClick={onDelete} color="error">
        <DeleteIcon fontSize="small" />
      </IconButton>
    </Box>
  );
};

interface ContractEditorProps {
  contract: ContractDefinition;
  onChange: (contract: ContractDefinition) => void;
  onDelete: () => void;
  index: number;
}

const ContractEditor = ({
  contract,
  onChange,
  onDelete,
  index,
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
        </Box>
      </AccordionSummary>
      <AccordionDetails sx={{ p: 3 }}>
        <Box sx={{ display: "flex", flexDirection: "column", gap: 2 }}>
          <Box sx={{ display: "flex", gap: 2 }}>
            <TextField
              size="small"
              label="Contract ID"
              value={contract.id}
              onChange={(e) => onChange({ ...contract, id: e.target.value })}
              fullWidth
            />
            <TextField
              size="small"
              label="Name"
              value={contract.name}
              onChange={(e) => onChange({ ...contract, name: e.target.value })}
              fullWidth
            />
          </Box>
          <TextField
            size="small"
            label="Package ID"
            value={contract.package_id}
            onChange={(e) =>
              onChange({ ...contract, package_id: e.target.value })
            }
            fullWidth
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
            />
          ))}
          <Button
            startIcon={<AddIcon />}
            onClick={handleAddField}
            variant="outlined"
            size="small"
          >
            Add Field
          </Button>
        </Box>
      </AccordionDetails>
    </Accordion>
  );
};

export const ContractsDialog = ({
  open,
  onClose,
  onComplete,
  partyId,
  participantIds,
}: ContractsDialogProps) => {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<ContractsStatusResponse | null>(null);

  // Form state - initialized with defaults from original contract-deploy.toml
  const [operatorParty, setOperatorParty] = useState(DEFAULT_OPERATOR_PARTY);
  const [operatorPartyHint, setOperatorPartyHint] = useState("operator");
  const [darFiles, setDarFiles] = useState<DarFile[]>([]);
  const [contracts, setContracts] =
    useState<ContractDefinition[]>(getDefaultContracts);

  useEffect(() => {
    if (!open) {
      setError(null);
      setStatus(null);
      setLoading(false);
    }
  }, [open]);

  const pollStatus = useCallback(async () => {
    try {
      const res = await fetch(`${API_BASE}/contracts/status`);
      if (res.ok) {
        const data: ContractsStatusResponse = await res.json();
        setStatus(data);
        if (data.status !== "inprogress") {
          setLoading(false);
          if (data.status === "completed") {
            onComplete();
          }
        }
      }
    } catch {
      // Ignore polling errors
    }
  }, [onComplete]);

  useEffect(() => {
    let interval: number | undefined;

    if (status?.status === "inprogress") {
      // Poll immediately, then every 2 seconds
      pollStatus();
      interval = window.setInterval(pollStatus, 2000);
    }

    return () => {
      if (interval) clearInterval(interval);
    };
  }, [status?.status, pollStatus]);

  const handleFileSelect = async (
    event: React.ChangeEvent<HTMLInputElement>,
  ) => {
    const files = event.target.files;
    if (!files) return;

    const newDarFiles: DarFile[] = [];

    for (let i = 0; i < files.length; i++) {
      const file = files[i];
      if (file.name.endsWith(".dar")) {
        const arrayBuffer = await file.arrayBuffer();
        const base64 = btoa(
          new Uint8Array(arrayBuffer).reduce(
            (data, byte) => data + String.fromCharCode(byte),
            "",
          ),
        );
        newDarFiles.push({
          filename: file.name,
          data: base64,
        });
      }
    }

    setDarFiles([...darFiles, ...newDarFiles]);
    // Reset input to allow selecting same files again
    event.target.value = "";
  };

  const handleRemoveDarFile = (index: number) => {
    setDarFiles(darFiles.filter((_, i) => i !== index));
  };

  const handleAddContract = () => {
    setContracts([...contracts, createEmptyContract()]);
  };

  const handleContractChange = (
    index: number,
    contract: ContractDefinition,
  ) => {
    const newContracts = [...contracts];
    newContracts[index] = contract;
    setContracts(newContracts);
  };

  const handleDeleteContract = (index: number) => {
    setContracts(contracts.filter((_, i) => i !== index));
  };

  const handleStart = async () => {
    setLoading(true);
    setError(null);

    try {
      const request: ContractsRequest = {
        decentralized_party_id: partyId,
        participant_ids: participantIds,
        operator_party: operatorParty || undefined,
        operator_party_hint: operatorPartyHint,
        dar_files: darFiles,
        contracts: contracts,
      };

      const res = await fetch(`${API_BASE}/contracts`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });

      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || "Failed to start contracts workflow");
      }

      setStatus({ status: "inprogress" });
    } catch (err) {
      setError(err instanceof Error ? err.message : "Unknown error");
      setLoading(false);
    }
  };

  const handleClose = () => {
    if (!loading) {
      onClose();
    }
  };

  const isInProgress = status?.status === "inprogress";
  const isCompleted = status?.status === "completed";
  const isFailed = status?.status === "failed";

  return (
    <Dialog open={open} onClose={handleClose} maxWidth="md" fullWidth>
      <DialogTitle>Deploy Contracts</DialogTitle>
      <DialogContent>
        <Box sx={{ display: "flex", flexDirection: "column", gap: 2, mt: 1 }}>
          <Typography variant="body2" color="text.secondary">
            Configure and deploy contracts for the decentralized party. This
            will coordinate with other participants to sign and execute the
            submissions.
          </Typography>

          {error && <Alert severity="error">{error}</Alert>}

          {isInProgress && (
            <Alert severity="info" icon={<CircularProgress size={20} />}>
              Contracts workflow in progress... This may take a few minutes.
            </Alert>
          )}

          {isCompleted && (
            <Alert severity="success">
              Contracts have been successfully deployed!
            </Alert>
          )}

          {isFailed && (
            <Alert severity="error">
              Contracts workflow failed: {status.error || "Unknown error"}
            </Alert>
          )}

          {!isInProgress && !isCompleted && (
            <>
              <Divider />
              <Typography variant="subtitle1">DAR Files</Typography>
              <Box
                sx={{
                  display: "flex",
                  flexDirection: "column",
                  gap: 1,
                }}
              >
                <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
                  <Button
                    component="label"
                    variant="outlined"
                    startIcon={<UploadFileIcon />}
                  >
                    Select DAR Files
                    <input
                      type="file"
                      hidden
                      multiple
                      accept=".dar"
                      onChange={handleFileSelect}
                    />
                  </Button>
                  <Typography variant="body2" color="text.secondary">
                    {darFiles.length === 0
                      ? "No files selected"
                      : `${darFiles.length} file(s) selected`}
                  </Typography>
                </Box>
                {darFiles.length > 0 && (
                  <Box
                    sx={{
                      display: "flex",
                      flexWrap: "wrap",
                      gap: 1,
                      p: 1,
                      border: "1px solid",
                      borderColor: "divider",
                      borderRadius: 1,
                    }}
                  >
                    {darFiles.map((file, index) => (
                      <Box
                        key={index}
                        sx={{
                          display: "flex",
                          alignItems: "center",
                          gap: 0.5,
                          px: 1,
                          py: 0.5,
                          bgcolor: "action.hover",
                          borderRadius: 1,
                        }}
                      >
                        <Typography variant="body2">{file.filename}</Typography>
                        <IconButton
                          size="small"
                          onClick={() => handleRemoveDarFile(index)}
                        >
                          <DeleteIcon fontSize="small" />
                        </IconButton>
                      </Box>
                    ))}
                  </Box>
                )}
              </Box>

              <Divider />
              <Typography variant="subtitle1">
                Operator Configuration
              </Typography>

              <TextField
                size="small"
                label="Operator Party ID (optional)"
                value={operatorParty}
                onChange={(e) => setOperatorParty(e.target.value)}
                fullWidth
                helperText="Leave empty to allocate a new operator party"
              />

              <TextField
                size="small"
                label="Operator Party Hint"
                value={operatorPartyHint}
                onChange={(e) => setOperatorPartyHint(e.target.value)}
                fullWidth
                helperText="Used when allocating a new operator party"
              />

              <Divider />
              <Box
                sx={{
                  display: "flex",
                  justifyContent: "space-between",
                  alignItems: "center",
                }}
              >
                <Typography variant="subtitle1">
                  Contract Definitions
                </Typography>
                <Button
                  startIcon={<AddIcon />}
                  onClick={handleAddContract}
                  variant="outlined"
                  size="small"
                >
                  Add Contract
                </Button>
              </Box>

              {contracts.length === 0 ? (
                <Typography variant="body2" color="text.secondary">
                  No contracts defined. Click "Add Contract" to define contracts
                  to deploy, or leave empty to skip contract creation.
                </Typography>
              ) : (
                contracts.map((contract, index) => (
                  <ContractEditor
                    key={index}
                    contract={contract}
                    onChange={(c) => handleContractChange(index, c)}
                    onDelete={() => handleDeleteContract(index)}
                    index={index}
                  />
                ))
              )}
            </>
          )}
        </Box>
      </DialogContent>
      <DialogActions>
        <Button onClick={handleClose} disabled={loading}>
          {isCompleted || isFailed ? "Close" : "Cancel"}
        </Button>
        {!status?.status || status.status === "idle" || isFailed ? (
          <Button
            onClick={handleStart}
            variant="contained"
            color="primary"
            disabled={loading}
          >
            {loading ? <CircularProgress size={20} /> : "Deploy Contracts"}
          </Button>
        ) : null}
      </DialogActions>
    </Dialog>
  );
};
