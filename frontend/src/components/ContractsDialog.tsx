import { useState, useEffect, useCallback, useMemo } from "react";
import {
  Autocomplete,
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
  Divider,
  Card,
  CardActionArea,
  CardContent,
  Tooltip,
} from "@mui/material";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import AddIcon from "@mui/icons-material/Add";
import DeleteIcon from "@mui/icons-material/Delete";
import ArrowBackIcon from "@mui/icons-material/ArrowBack";
import AccountBalanceIcon from "@mui/icons-material/AccountBalance";
import GavelIcon from "@mui/icons-material/Gavel";
import HandymanIcon from "@mui/icons-material/Handyman";
import LockIcon from "@mui/icons-material/Lock";
import StorageIcon from "@mui/icons-material/Storage";
import { API_BASE } from "../constants";
import { authenticatedFetch } from "../api";
import { useSnackbar } from "../contexts";
import type {
  ContractsStatusResponse,
  ContractsRequest,
  ContractDefinition,
  ContractInfo,
  FieldDefinition,
  PackageConfig,
  VettedPackageInfo,
} from "../types";

interface ContractsDialogProps {
  open: boolean;
  onClose: () => void;
  onComplete: () => void;
  partyId: string;
  participantIds: string[];
  defaultOperatorParty?: string;
  knownPackageIds?: string[];
  deployedContracts?: ContractInfo[];
  vettedPackages?: VettedPackageInfo[];
}

type ContractType = "governance-core" | "cbtc" | "vault" | null;

const FIELD_TYPES = [
  { value: "decentralized_party", label: "Dec. Party" },
  { value: "operator_party", label: "Operator" },
  { value: "participant_party", label: "Party" },
  { value: "party_set", label: "Member Set" },
  { value: "attestors_set", label: "Attestor Set" },
  { value: "governance_threshold", label: "Threshold" },
  { value: "rel_time", label: "Proposal Timeout" },
  { value: "optional", label: "Optional" },
  { value: "instrument", label: "Instrument" },
  { value: "text", label: "Text" },
  { value: "int64", label: "Integer" },
  { value: "bool", label: "Boolean" },
  { value: "record", label: "Record" },
];

const createDefaultField = (
  type: string,
  participantCount: number = 3,
): FieldDefinition => {
  const defaultThreshold = Math.max(2, Math.ceil((participantCount * 2) / 3));
  switch (type) {
    case "decentralized_party":
      return { type: "decentralized_party" };
    case "operator_party":
      return { type: "operator_party" };
    case "participant_party":
      return { type: "participant_party", id: "" };
    case "party_set":
      return { type: "party_set", parties: [] };
    case "attestors_set":
      return { type: "attestors_set" };
    case "governance_threshold":
      return { type: "governance_threshold", value: defaultThreshold };
    case "rel_time":
      return { type: "rel_time", microseconds: 3600000000 };
    case "optional":
      return {
        type: "optional",
        inner: { type: "rel_time", microseconds: 3600000000 },
      };
    case "instrument":
      return { type: "instrument", id: "" };
    case "text":
      return { type: "text", value: "" };
    case "int64":
      return { type: "int64", value: 0 };
    case "bool":
      return { type: "bool", value: false };
    case "record":
      return { type: "record", fields: [] };
    default:
      return { type: "text", value: "" };
  }
};

const createEmptyContract = (): ContractDefinition => ({
  id: "",
  name: "",
  package_id: "",
  module_name: "",
  entity_name: "",
  fields: [],
});

// Governance Core contract definitions
const getGovernanceCoreContracts = (
  participantCount: number = 3,
  governanceCorePkg: string = "",
): ContractDefinition[] => {
  const defaultThreshold = Math.max(2, Math.ceil((participantCount * 2) / 3));
  return [
    {
      id: "create-governance-rules",
      name: "GovernanceRules",
      package_id: governanceCorePkg,
      module_name: "Governance.Rules",
      entity_name: "GovernanceRules",
      fields: [
        { type: "decentralized_party" }, // governanceParty : Party
        { type: "party_set", parties: [] }, // members : Set Party
        { type: "governance_threshold", value: defaultThreshold }, // threshold : Int
        { type: "rel_time", microseconds: 86400000000 }, // actionConfirmationTimeout : RelTime (24 hours)
        // additionalProposers : Optional (Set Party). Defaults to Some(empty
        // set) — functionally equivalent to None ("no allowlist") at the
        // contract level. Operators can add proposer parties up front, or
        // grow the set later via the Add/Remove governance self-actions.
        { type: "optional", inner: { type: "party_set", parties: [] } },
      ],
      fieldLabels: [
        "Dec. Party",
        "Member Set",
        "Threshold",
        "Proposal Timeout",
        "Additional Proposers",
      ],
    },
  ];
};

// CBTC contract definitions
const getCbtcContracts = (): ContractDefinition[] => [
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

// Vault contract definitions
const getVaultContracts = (
  participantCount: number = 3,
  vaultGovernancePkg: string = "",
): ContractDefinition[] => {
  const defaultThreshold = Math.max(2, Math.ceil((participantCount * 2) / 3));
  return [
    {
      id: "create-vault-governance-rules",
      name: "VaultGovernanceRules",
      package_id: vaultGovernancePkg,
      module_name: "BitsafeVault.VaultGovernance",
      entity_name: "VaultGovernanceRules",
      fields: [
        { type: "decentralized_party" }, // vaultManager : Party
        { type: "party_set", parties: [] }, // members : Set Party - add parties manually
        { type: "governance_threshold", value: defaultThreshold }, // threshold : Int
        {
          type: "optional",
          inner: { type: "rel_time", microseconds: 86400000000 },
        }, // actionConfirmationTimeout : Optional RelTime (24 hours)
      ],
    },
  ];
};

const getContractsForType = (
  type: ContractType,
  participantCount: number = 3,
  packages?: PackageConfig,
): ContractDefinition[] => {
  switch (type) {
    case "governance-core":
      return getGovernanceCoreContracts(
        participantCount,
        packages?.governance_core ?? "",
      );
    case "cbtc":
      return getCbtcContracts();
    case "vault":
      return getVaultContracts(
        participantCount,
        packages?.vault_governance ?? "",
      );
    default:
      return [];
  }
};

interface FieldEditorProps {
  field: FieldDefinition;
  onChange: (field: FieldDefinition) => void;
  onDelete: () => void;
  participantCount?: number;
  partyId?: string;
  lockStructure?: boolean;
  /** Override the type-derived label (e.g. show "Additional Proposers"
   *  instead of the generic "Optional"). Only honored when `lockStructure`
   *  is true. */
  label?: string;
}

const FieldEditor = ({
  field,
  onChange,
  onDelete,
  participantCount = 3,
  partyId,
  lockStructure = false,
  label,
}: FieldEditorProps) => {
  const defaultThreshold = Math.max(2, Math.ceil((participantCount * 2) / 3));

  const renderValueInput = () => {
    switch (field.type) {
      case "decentralized_party":
        return (
          <Typography
            variant="body2"
            color="text.secondary"
            sx={{
              fontFamily: "monospace",
              fontSize: 14,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
              width: "100%",
            }}
          >
            {partyId || "(dec party ID)"}
          </Typography>
        );
      case "operator_party":
        return (
          <Typography
            variant="body2"
            color="text.secondary"
            sx={{ fontStyle: "italic" }}
          >
            (auto-allocated)
          </Typography>
        );
      case "attestors_set":
        return (
          <Typography
            variant="body2"
            color="text.secondary"
            sx={{ fontStyle: "italic" }}
          >
            (all {participantCount} participants)
          </Typography>
        );

      case "participant_party":
        return (
          <TextField
            size="small"
            placeholder="Paste party ID"
            value={field.id}
            onChange={(e) => onChange({ ...field, id: e.target.value })}
            fullWidth
          />
        );

      case "party_set":
        return (
          <Box
            sx={{ display: "flex", flexDirection: "column", gap: 1, flex: 1 }}
          >
            <TextField
              size="small"
              placeholder="Paste party ID, press Enter"
              fullWidth
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  const input = e.target as HTMLInputElement;
                  const value = input.value.trim();
                  if (value && !field.parties.includes(value)) {
                    onChange({ ...field, parties: [...field.parties, value] });
                    input.value = "";
                  }
                  e.preventDefault();
                }
              }}
            />
            {field.parties.length > 0 && (
              <Box sx={{ display: "flex", flexDirection: "column", gap: 0.5 }}>
                {field.parties.map((party, idx) => (
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
                      sx={{ flex: 1, fontFamily: "monospace" }}
                    >
                      {party}
                    </Typography>
                    <IconButton
                      size="small"
                      onClick={() =>
                        onChange({
                          ...field,
                          parties: field.parties.filter((_, i) => i !== idx),
                        })
                      }
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

      case "governance_threshold":
        return (
          <TextField
            size="small"
            label="Value"
            type="number"
            value={field.value ?? defaultThreshold}
            onChange={(e) =>
              onChange({
                ...field,
                value: parseInt(e.target.value) || defaultThreshold,
              })
            }
            sx={{ width: 100 }}
          />
        );

      case "rel_time":
        return (
          <FormControl size="small" sx={{ width: 130 }}>
            <Select
              value={field.microseconds || 3600000000}
              onChange={(e) =>
                onChange({ ...field, microseconds: Number(e.target.value) })
              }
            >
              <MenuItem value={180000000}>3 min</MenuItem>
              <MenuItem value={600000000}>10 min</MenuItem>
              <MenuItem value={1800000000}>30 min</MenuItem>
              <MenuItem value={3600000000}>1 hour</MenuItem>
              <MenuItem value={7200000000}>2 hours</MenuItem>
              <MenuItem value={86400000000}>24 hours</MenuItem>
            </Select>
          </FormControl>
        );

      case "optional": {
        const innerTypes = FIELD_TYPES.filter(
          (t) => t.value !== "optional" && t.value !== "record",
        );
        return (
          <Box sx={{ display: "flex", gap: 1, alignItems: "center", flex: 1 }}>
            {!lockStructure && (
              <FormControl size="small" sx={{ width: 140 }}>
                <Select
                  value={field.inner?.type || "rel_time"}
                  onChange={(e) =>
                    onChange({
                      ...field,
                      inner: createDefaultField(
                        e.target.value,
                        participantCount,
                      ),
                    })
                  }
                >
                  {innerTypes.map((t) => (
                    <MenuItem key={t.value} value={t.value}>
                      {t.label}
                    </MenuItem>
                  ))}
                </Select>
              </FormControl>
            )}
            {field.inner?.type === "rel_time" && (
              <FormControl size="small" sx={{ width: 130 }}>
                <Select
                  value={
                    (field.inner as { microseconds: number }).microseconds ||
                    3600000000
                  }
                  onChange={(e) =>
                    onChange({
                      ...field,
                      inner: {
                        type: "rel_time",
                        microseconds: Number(e.target.value),
                      },
                    })
                  }
                >
                  <MenuItem value={180000000}>3 min</MenuItem>
                  <MenuItem value={600000000}>10 min</MenuItem>
                  <MenuItem value={1800000000}>30 min</MenuItem>
                  <MenuItem value={3600000000}>1 hour</MenuItem>
                  <MenuItem value={7200000000}>2 hours</MenuItem>
                  <MenuItem value={86400000000}>24 hours</MenuItem>
                </Select>
              </FormControl>
            )}
            {field.inner?.type === "governance_threshold" && (
              <TextField
                size="small"
                label="Value"
                type="number"
                value={
                  (field.inner as { value?: number }).value ?? defaultThreshold
                }
                onChange={(e) =>
                  onChange({
                    ...field,
                    inner: {
                      type: "governance_threshold",
                      value: parseInt(e.target.value) || defaultThreshold,
                    },
                  })
                }
                sx={{ width: 100 }}
              />
            )}
            {field.inner?.type === "text" && (
              <TextField
                size="small"
                label="Value"
                value={(field.inner as { value: string }).value}
                onChange={(e) =>
                  onChange({
                    ...field,
                    inner: { type: "text", value: e.target.value },
                  })
                }
                sx={{ flex: 1 }}
              />
            )}
            {field.inner?.type === "int64" && (
              <TextField
                size="small"
                label="Value"
                type="number"
                value={(field.inner as { value: number }).value}
                onChange={(e) =>
                  onChange({
                    ...field,
                    inner: {
                      type: "int64",
                      value: parseInt(e.target.value) || 0,
                    },
                  })
                }
                sx={{ width: 100 }}
              />
            )}
            {field.inner?.type === "party_set" && (
              <Box
                sx={{
                  display: "flex",
                  flexDirection: "column",
                  gap: 1,
                  flex: 1,
                }}
              >
                <TextField
                  size="small"
                  placeholder="Paste party ID, press Enter (leave empty for no extra proposers)"
                  fullWidth
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      const input = e.target as HTMLInputElement;
                      const value = input.value.trim();
                      const inner = field.inner as { parties: string[] };
                      if (value && !inner.parties.includes(value)) {
                        onChange({
                          ...field,
                          inner: {
                            type: "party_set",
                            parties: [...inner.parties, value],
                          },
                        });
                        input.value = "";
                      }
                      e.preventDefault();
                    }
                  }}
                />
                {(field.inner as { parties: string[] }).parties.length > 0 && (
                  <Box
                    sx={{ display: "flex", flexDirection: "column", gap: 0.5 }}
                  >
                    {(field.inner as { parties: string[] }).parties.map(
                      (party, idx) => (
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
                            sx={{ flex: 1, fontFamily: "monospace" }}
                          >
                            {party}
                          </Typography>
                          <IconButton
                            size="small"
                            onClick={() => {
                              const inner = field.inner as {
                                parties: string[];
                              };
                              onChange({
                                ...field,
                                inner: {
                                  type: "party_set",
                                  parties: inner.parties.filter(
                                    (_, i) => i !== idx,
                                  ),
                                },
                              });
                            }}
                            sx={{ p: 0.25 }}
                          >
                            <DeleteIcon sx={{ fontSize: 14 }} />
                          </IconButton>
                        </Box>
                      ),
                    )}
                  </Box>
                )}
              </Box>
            )}
          </Box>
        );
      }

      case "instrument":
        return (
          <TextField
            size="small"
            label="ID"
            value={field.id}
            onChange={(e) => onChange({ ...field, id: e.target.value })}
            sx={{ width: 150 }}
          />
        );

      case "text":
        return (
          <TextField
            size="small"
            label="Value"
            value={field.value}
            onChange={(e) => onChange({ ...field, value: e.target.value })}
            sx={{ flex: 1 }}
          />
        );

      case "int64":
        return (
          <TextField
            size="small"
            label="Value"
            type="number"
            value={field.value}
            onChange={(e) =>
              onChange({ ...field, value: parseInt(e.target.value) || 0 })
            }
            sx={{ width: 120 }}
          />
        );

      case "bool":
        return (
          <FormControl size="small" sx={{ width: 100 }}>
            <Select
              value={field.value ? "true" : "false"}
              onChange={(e) =>
                onChange({ ...field, value: e.target.value === "true" })
              }
            >
              <MenuItem value="true">True</MenuItem>
              <MenuItem value="false">False</MenuItem>
            </Select>
          </FormControl>
        );

      case "record":
        return (
          <Box
            sx={{ display: "flex", flexDirection: "column", gap: 1, flex: 1 }}
          >
            <Box
              sx={{
                border: "1px solid",
                borderColor: "divider",
                borderRadius: 1,
                p: 1.5,
                bgcolor: "action.hover",
              }}
            >
              {field.fields.length === 0 ? (
                <Typography
                  variant="body2"
                  color="text.secondary"
                  sx={{ fontStyle: "italic" }}
                >
                  Empty record
                </Typography>
              ) : (
                <Box sx={{ display: "flex", flexDirection: "column", gap: 1 }}>
                  {field.fields.map((nestedField, idx) => (
                    <Box
                      key={idx}
                      sx={{ display: "flex", gap: 1, alignItems: "center" }}
                    >
                      <FormControl size="small" sx={{ width: 130 }}>
                        <Select
                          value={nestedField.type}
                          onChange={(e) => {
                            const newFields = [...field.fields];
                            newFields[idx] = createDefaultField(
                              e.target.value,
                              participantCount,
                            );
                            onChange({ ...field, fields: newFields });
                          }}
                        >
                          {FIELD_TYPES.filter((t) => t.value !== "record").map(
                            (t) => (
                              <MenuItem key={t.value} value={t.value}>
                                {t.label}
                              </MenuItem>
                            ),
                          )}
                        </Select>
                      </FormControl>
                      <IconButton
                        size="small"
                        onClick={() => {
                          const newFields = field.fields.filter(
                            (_, i) => i !== idx,
                          );
                          onChange({ ...field, fields: newFields });
                        }}
                        color="error"
                      >
                        <DeleteIcon sx={{ fontSize: 16 }} />
                      </IconButton>
                    </Box>
                  ))}
                </Box>
              )}
              <Button
                size="small"
                startIcon={<AddIcon />}
                onClick={() =>
                  onChange({
                    ...field,
                    fields: [...field.fields, { type: "text", value: "" }],
                  })
                }
                sx={{ mt: 1 }}
              >
                Add
              </Button>
            </Box>
          </Box>
        );

      default:
        return null;
    }
  };

  return (
    <Box
      sx={{
        display: "grid",
        gridTemplateColumns: "150px 1fr 40px",
        gap: 1.5,
        alignItems: "center",
        py: 1,
      }}
    >
      {lockStructure ? (
        <Typography variant="body2" sx={{ fontWeight: 500, pl: 1.5, py: 1 }}>
          {label ??
            FIELD_TYPES.find((t) => t.value === field.type)?.label ??
            field.type}
        </Typography>
      ) : (
        <FormControl size="small" fullWidth>
          <Select
            value={field.type}
            onChange={(e) =>
              onChange(createDefaultField(e.target.value, participantCount))
            }
          >
            {FIELD_TYPES.map((t) => (
              <MenuItem key={t.value} value={t.value}>
                {t.label}
              </MenuItem>
            ))}
          </Select>
        </FormControl>
      )}
      <Box sx={{ display: "flex", alignItems: "center", minWidth: 0 }}>
        {renderValueInput()}
      </Box>
      {!lockStructure && (
        <IconButton size="small" onClick={onDelete} color="error">
          <DeleteIcon fontSize="small" />
        </IconButton>
      )}
    </Box>
  );
};

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
            renderInput={(params) => (
              <TextField
                {...params}
                label="Package Name / ID"
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

// Plugin card with optional tooltip and enabled/disabled visual state
const PluginCard = ({
  icon,
  label,
  description,
  onClick,
  tooltip,
  enabled = false,
}: {
  icon: React.ReactNode;
  label: string;
  description: string;
  onClick?: () => void;
  tooltip?: string;
  enabled?: boolean;
}) => {
  const card = (
    <Card
      sx={{
        flex: 1,
        border: 1,
        borderColor: "divider",
        opacity: enabled ? 1 : 0.5,
        "&:hover": onClick ? { borderColor: "primary.main" } : {},
      }}
    >
      <CardActionArea
        disabled={!onClick}
        onClick={onClick}
        sx={{ p: 2, height: "100%" }}
      >
        <CardContent sx={{ textAlign: "center" }}>
          {icon}
          <Typography variant="h6">{label}</Typography>
          <Typography variant="body2" color="text.secondary">
            {description}
          </Typography>
        </CardContent>
      </CardActionArea>
    </Card>
  );
  return tooltip ? (
    <Tooltip title={tooltip} arrow>
      {card}
    </Tooltip>
  ) : (
    card
  );
};

// Contract type selection screen
interface ContractTypeSelectionProps {
  onSelect: (type: ContractType) => void;
  isGovernanceCoreDeployed: boolean;
  isGovernanceCoreDarUploaded: boolean;
  isTokenCustodyDarUploaded: boolean;
  isUtilityCredentialPluginDarUploaded: boolean;
}

const ContractTypeSelection = ({
  onSelect,
  isGovernanceCoreDeployed,
  isGovernanceCoreDarUploaded,
  isTokenCustodyDarUploaded,
  isUtilityCredentialPluginDarUploaded,
}: ContractTypeSelectionProps) => {
  return (
    <Box sx={{ display: "flex", flexDirection: "column", gap: 2 }}>
      {/* Deploy Governance Core — shown only when not yet deployed */}
      {!isGovernanceCoreDeployed && (
        <>
          <Typography variant="body2" color="text.secondary">
            Deploy the core governance contract for this decentralized party.
          </Typography>
          <Box sx={{ display: "flex", gap: 2, mt: 1 }}>
            <Tooltip
              title={
                isGovernanceCoreDarUploaded
                  ? ""
                  : "Upload the governance-core DAR first"
              }
              arrow
            >
              <Card
                sx={{
                  flex: 1,
                  maxWidth: 280,
                  border: 1,
                  borderColor: "divider",
                  opacity: isGovernanceCoreDarUploaded ? 1 : 0.5,
                  "&:hover": isGovernanceCoreDarUploaded
                    ? { borderColor: "primary.main" }
                    : {},
                }}
              >
                <CardActionArea
                  onClick={() => onSelect("governance-core")}
                  disabled={!isGovernanceCoreDarUploaded}
                  sx={{ p: 2, height: "100%" }}
                >
                  <CardContent sx={{ textAlign: "center" }}>
                    <GavelIcon
                      sx={{ fontSize: 48, color: "primary.main", mb: 1 }}
                    />
                    <Typography variant="h6">Governance Core</Typography>
                    <Typography variant="body2" color="text.secondary">
                      Deploy core governance rules contract
                    </Typography>
                  </CardContent>
                </CardActionArea>
              </Card>
            </Tooltip>
          </Box>
        </>
      )}

      {/* Plugins — shown only when governance core is deployed */}
      {isGovernanceCoreDeployed && (
        <>
          <Typography variant="body2" color="text.secondary">
            Governance Core is deployed. Available plugins:
          </Typography>
          <Box
            sx={{
              display: "grid",
              gridTemplateColumns: "repeat(2, 1fr)",
              gap: 2,
              mt: 1,
            }}
          >
            <PluginCard
              icon={
                <LockIcon sx={{ fontSize: 48, color: "primary.main", mb: 1 }} />
              }
              label="Token Custody"
              description="Receive, transfer, and manage tokens via governance"
              enabled={isTokenCustodyDarUploaded}
              tooltip={
                isTokenCustodyDarUploaded
                  ? undefined
                  : "Upload the governance-token-custody DAR first"
              }
            />
            <PluginCard
              icon={
                <AccountBalanceIcon
                  sx={{ fontSize: 48, color: "primary.main", mb: 1 }}
                />
              }
              label="CBTC"
              description="Bitcoin-backed deposit and withdrawal management"
              onClick={() => onSelect("cbtc")}
              tooltip="Coming soon"
            />
            <PluginCard
              icon={
                <StorageIcon
                  sx={{ fontSize: 48, color: "primary.main", mb: 1 }}
                />
              }
              label="Vault"
              description="Pooled custody with yield and share accounting"
              onClick={() => onSelect("vault")}
              tooltip="Coming soon"
            />
            <PluginCard
              icon={
                <HandymanIcon
                  sx={{ fontSize: 48, color: "primary.main", mb: 1 }}
                />
              }
              label="Utility"
              description="Registry services and verifiable credentials"
              tooltip={
                isUtilityCredentialPluginDarUploaded
                  ? "Credential plugin DAR uploaded — propose Offer/Accept Free credential actions from the Governance section."
                  : "Coming soon"
              }
            />
            <PluginCard
              icon={
                <AddIcon sx={{ fontSize: 48, color: "primary.main", mb: 1 }} />
              }
              label="Add Plugin"
              description="Upload a custom plugin DAR from the Packages tab"
              tooltip="Go to the Packages tab to upload DARs"
            />
          </Box>
        </>
      )}
    </Box>
  );
};

export const ContractsDialog = ({
  open,
  onClose,
  onComplete,
  partyId,
  participantIds,
  defaultOperatorParty,
  knownPackageIds = [],
  deployedContracts = [],
  vettedPackages: initialVettedPackages = [],
}: ContractsDialogProps) => {
  const [vettedPackages, setVettedPackages] = useState(initialVettedPackages);

  // Fetch fresh vetted packages when dialog opens
  useEffect(() => {
    if (open) {
      authenticatedFetch(`${API_BASE}/packages/vetted`)
        .then((res) => (res.ok ? res.json() : []))
        .then((data: VettedPackageInfo[]) => setVettedPackages(data))
        .catch(() => {});
    }
  }, [open]);

  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<ContractsStatusResponse | null>(null);
  const [contractType, setContractType] = useState<ContractType>(null);
  const { showSnackbar } = useSnackbar();

  // Package config from API
  const [packages, setPackages] = useState<PackageConfig>({});

  // Form state
  const [operatorParty, setOperatorParty] = useState(
    defaultOperatorParty || "",
  );
  // Sync the autofetched operator party (from App.tsx) into local state once
  // it arrives — without this, the field stays empty whenever the fetch
  // completes after this component has already mounted with an empty default.
  useEffect(() => {
    if (defaultOperatorParty) setOperatorParty(defaultOperatorParty);
  }, [defaultOperatorParty]);
  const [participantParties, setParticipantParties] = useState<string[]>([]);
  const [contracts, setContracts] = useState<ContractDefinition[]>([]);

  // Governance Core deployment state
  const isGovernanceCoreDeployed = deployedContracts.some(
    (c) => c.template_id === "Governance.Rules:GovernanceRules",
  );
  const isDarUploaded = (patterns: string[]) =>
    vettedPackages.some((p) => {
      const name = p.package_name.toLowerCase();
      return patterns.some((pat) => name.includes(pat));
    });

  const isGovernanceCoreDarUploaded = isDarUploaded([
    "governance-core",
    "governance.rules",
  ]);
  const isTokenCustodyDarUploaded = isDarUploaded([
    "governance-token-custody",
    "tokencustody",
  ]);
  const isUtilityCredentialPluginDarUploaded = isDarUploaded([
    "governance-utility-credential",
  ]);

  // Combine package IDs from config + known contracts for dropdown
  const allPackageIds = useMemo(() => {
    const ids = new Set(knownPackageIds);
    if (packages.governance_core) ids.add(packages.governance_core);
    if (packages.vault_governance) ids.add(packages.vault_governance);
    if (packages.vault) ids.add(packages.vault);
    if (packages.utility_registry) ids.add(packages.utility_registry);
    if (packages.utility_credential) ids.add(packages.utility_credential);
    return [...ids].sort();
  }, [knownPackageIds, packages]);

  // Fetch packages config when dialog opens
  useEffect(() => {
    if (open && partyId) {
      authenticatedFetch(
        `${API_BASE}/packages?party_id=${encodeURIComponent(partyId)}`,
      )
        .then((res) => res.json())
        .then((data: PackageConfig) => setPackages(data))
        .catch((e) => console.warn("Failed to fetch packages:", e));
    }
  }, [open, partyId]);

  // Reset state when dialog opens/closes
  useEffect(() => {
    if (!open) {
      setError(null);
      setStatus(null);
      setLoading(false);
      setContractType(null);
      setContracts([]);
      setOperatorParty(defaultOperatorParty || "");
      setParticipantParties([]);
      setPackages({});
    }
  }, [open, defaultOperatorParty]);

  // Initialize contracts when type is selected
  useEffect(() => {
    if (contractType) {
      setContracts(
        getContractsForType(contractType, participantIds.length, packages),
      );
    }
  }, [contractType, participantIds.length, packages]);

  // Pre-fill the Member Set for gov-core from each peer's configured member_party_id.
  useEffect(() => {
    if (contractType !== "governance-core") return;
    let cancelled = false;
    const fetchKnownMembers = async () => {
      try {
        const res = await authenticatedFetch(
          `${API_BASE}/governance/known-members?party_id=${encodeURIComponent(
            partyId,
          )}`,
        );
        if (!res.ok || cancelled) return;
        const data: {
          members: Array<{ participant_uid: string; member_party_id?: string }>;
        } = await res.json();
        const memberIds = data.members
          .map((m) => m.member_party_id)
          .filter((id): id is string => !!id && id.length > 0);
        if (cancelled || memberIds.length === 0) return;
        setContracts((prev) =>
          prev.map((c) => ({
            ...c,
            fields: c.fields.map((f) =>
              f.type === "party_set" ? { ...f, parties: memberIds } : f,
            ),
          })),
        );
      } catch {
        // If discovery fails, the operator can still type values manually.
      }
    };
    fetchKnownMembers();
    return () => {
      cancelled = true;
    };
  }, [contractType, partyId, packages]);

  const pollStatus = useCallback(async () => {
    try {
      const res = await authenticatedFetch(`${API_BASE}/contracts/status`);
      if (res.ok) {
        const data: ContractsStatusResponse = await res.json();
        if (data.status === "cancelled") {
          showSnackbar("Contracts workflow cancelled");
          onClose();
          return;
        }
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
  }, [onComplete, onClose, showSnackbar]);

  useEffect(() => {
    let interval: number | undefined;

    if (status?.status === "inprogress") {
      pollStatus();
      interval = window.setInterval(pollStatus, 2000);
    }

    return () => {
      if (interval) clearInterval(interval);
    };
  }, [status?.status, pollStatus]);

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

    // Validate required fields
    if (
      contractType !== "vault" &&
      contractType !== "governance-core" &&
      !operatorParty
    ) {
      setError("Operator party ID is required");
      setLoading(false);
      return;
    }

    if (
      contractType !== "vault" &&
      contractType !== "governance-core" &&
      participantParties.length !== participantIds.length
    ) {
      setError(
        `Please provide party IDs for all ${participantIds.length} participants`,
      );
      setLoading(false);
      return;
    }

    // For gov core the user doesn't fill the top section; derive participant_parties from the contract's Member Set field.
    const submittedParticipantParties =
      contractType === "governance-core"
        ? (contracts
            .flatMap((c) => c.fields)
            .find((f) => f.type === "party_set")?.parties ?? [])
        : participantParties;

    try {
      const request: ContractsRequest = {
        decentralized_party_id: partyId,
        participant_ids: participantIds,
        participant_parties: submittedParticipantParties,
        operator_party: operatorParty,
        contracts: contracts,
      };

      const res = await authenticatedFetch(`${API_BASE}/contracts`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });

      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || "Failed to start contracts workflow");
      }

      showSnackbar("Contracts workflow started — follow progress in the feed");
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Unknown error");
      setLoading(false);
    }
  };

  const [cancelling, setCancelling] = useState(false);
  const handleCancelWorkflow = async () => {
    setCancelling(true);
    try {
      const res = await authenticatedFetch(`${API_BASE}/contracts/cancel`, {
        method: "POST",
      });
      if (res.ok) {
        showSnackbar("Contracts workflow cancelled");
        onClose();
      } else {
        const data = await res.json().catch(() => ({}));
        setError(data.error || "Failed to cancel workflow");
      }
    } catch (err) {
      setError(
        err instanceof Error ? err.message : "Failed to cancel workflow",
      );
    } finally {
      setCancelling(false);
    }
  };

  const handleClose = () => {
    if (!loading) {
      onClose();
    }
  };

  const handleBack = () => {
    setContractType(null);
    setContracts([]);
  };

  const isInProgress = status?.status === "inprogress";
  const isCompleted = status?.status === "completed";
  const isFailed = status?.status === "failed";

  const getDialogTitle = () => {
    if (!contractType)
      return isGovernanceCoreDeployed ? "Plugin Manager" : "Deploy Contracts";
    if (contractType === "governance-core") return "Deploy Governance Core";
    if (contractType === "cbtc") return "Deploy CBTC Contracts";
    return "Deploy Vault Contracts";
  };

  return (
    <Dialog open={open} onClose={handleClose} maxWidth="md" fullWidth>
      <DialogTitle>
        <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
          {contractType && !isInProgress && !isCompleted && (
            <IconButton size="small" onClick={handleBack} sx={{ mr: 1 }}>
              <ArrowBackIcon />
            </IconButton>
          )}
          {getDialogTitle()}
        </Box>
      </DialogTitle>
      <DialogContent>
        <Box sx={{ display: "flex", flexDirection: "column", gap: 2, mt: 1 }}>
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

          {!isInProgress && !isCompleted && !contractType && (
            <ContractTypeSelection
              onSelect={setContractType}
              isGovernanceCoreDeployed={isGovernanceCoreDeployed}
              isGovernanceCoreDarUploaded={isGovernanceCoreDarUploaded}
              isTokenCustodyDarUploaded={isTokenCustodyDarUploaded}
              isUtilityCredentialPluginDarUploaded={
                isUtilityCredentialPluginDarUploaded
              }
            />
          )}

          {!isInProgress && !isCompleted && contractType && (
            <>
              <Typography variant="body2" color="text.secondary">
                Configure and deploy contracts for the decentralized party. This
                will coordinate with other participants to sign and execute the
                submissions. Make sure DARs have been uploaded first.
              </Typography>

              {contractType !== "vault" &&
                contractType !== "governance-core" && (
                  <>
                    <Divider />
                    <Typography variant="subtitle1">
                      Party Configuration
                    </Typography>
                  </>
                )}

              {contractType !== "vault" &&
                contractType !== "governance-core" && (
                  <TextField
                    size="small"
                    label="Operator Party ID"
                    value={operatorParty}
                    onChange={(e) => setOperatorParty(e.target.value)}
                    fullWidth
                    required
                    error={!operatorParty}
                    helperText="Full party ID for the operator (e.g., operator::1220...)"
                  />
                )}

              {contractType !== "governance-core" && (
                <>
                  <Typography variant="subtitle2" sx={{ mt: 1 }}>
                    Participant Party IDs ({participantParties.length}/
                    {participantIds.length})
                  </Typography>
                  <Typography
                    variant="body2"
                    color="text.secondary"
                    sx={{ mb: 1 }}
                  >
                    Enter the party ID for each participant. Must match the
                    order of participant IDs.
                  </Typography>
                  <Box
                    sx={{ display: "flex", flexDirection: "column", gap: 1 }}
                  >
                    <TextField
                      size="small"
                      placeholder="Paste party ID, press Enter"
                      fullWidth
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          const input = e.target as HTMLInputElement;
                          const value = input.value.trim();
                          if (
                            value &&
                            participantParties.length < participantIds.length
                          ) {
                            setParticipantParties([
                              ...participantParties,
                              value,
                            ]);
                            input.value = "";
                          }
                          e.preventDefault();
                        }
                      }}
                      disabled={
                        participantParties.length >= participantIds.length
                      }
                    />
                    {participantParties.length > 0 && (
                      <Box
                        sx={{
                          display: "flex",
                          flexDirection: "column",
                          gap: 0.5,
                        }}
                      >
                        {participantParties.map((party, idx) => (
                          <Box
                            key={idx}
                            sx={{
                              display: "flex",
                              alignItems: "center",
                              bgcolor: "action.hover",
                              borderRadius: 1,
                              px: 1,
                              py: 0.5,
                            }}
                          >
                            <Typography
                              variant="caption"
                              color="text.secondary"
                              sx={{ mr: 1, minWidth: 20 }}
                            >
                              {idx + 1}.
                            </Typography>
                            <Typography
                              variant="caption"
                              sx={{
                                flex: 1,
                                fontFamily: "monospace",
                                overflow: "hidden",
                                textOverflow: "ellipsis",
                              }}
                            >
                              {party}
                            </Typography>
                            <IconButton
                              size="small"
                              onClick={() =>
                                setParticipantParties(
                                  participantParties.filter(
                                    (_, i) => i !== idx,
                                  ),
                                )
                              }
                              sx={{ p: 0.25 }}
                            >
                              <DeleteIcon sx={{ fontSize: 14 }} />
                            </IconButton>
                          </Box>
                        ))}
                      </Box>
                    )}
                  </Box>
                </>
              )}

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
                {contractType !== "governance-core" && (
                  <Button
                    startIcon={<AddIcon />}
                    onClick={handleAddContract}
                    variant="outlined"
                    size="small"
                  >
                    Add Contract
                  </Button>
                )}
              </Box>

              {contracts.length === 0 ? (
                <Typography variant="body2" color="text.secondary">
                  {contractType === "governance-core"
                    ? "No contracts to deploy."
                    : 'No contracts defined. Click "Add Contract" to define contracts to deploy, or leave empty to skip contract creation.'}
                </Typography>
              ) : (
                contracts.map((contract, index) => (
                  <ContractEditor
                    key={index}
                    contract={contract}
                    onChange={(c) => handleContractChange(index, c)}
                    onDelete={() => handleDeleteContract(index)}
                    index={index}
                    participantCount={participantIds.length}
                    partyId={partyId}
                    knownPackageIds={allPackageIds}
                    lockStructure={contractType === "governance-core"}
                  />
                ))
              )}
            </>
          )}
        </Box>
      </DialogContent>
      <DialogActions>
        <Button onClick={handleClose} disabled={loading}>
          {isCompleted || isFailed || isInProgress ? "Close" : "Cancel"}
        </Button>
        {isInProgress && (
          <Button
            onClick={handleCancelWorkflow}
            variant="outlined"
            color="error"
            disabled={cancelling}
            startIcon={cancelling ? <CircularProgress size={16} /> : undefined}
          >
            {cancelling ? "Cancelling…" : "Cancel Workflow"}
          </Button>
        )}
        {contractType &&
        (!status?.status || status.status === "idle" || isFailed) ? (
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
