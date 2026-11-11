import { Button, Typography, Box, TextField, IconButton, Select, MenuItem, FormControl } from "@mui/material";
import AddIcon from "@mui/icons-material/Add";
import DeleteIcon from "@mui/icons-material/Delete";
import { TextHelp, fieldHelpAdornment } from "../FieldHelp";
import type { FieldDefinition } from "../../types";
import { FIELD_TYPES, FIELD_TYPE_HELP, createDefaultField, commitPartyInput } from "./contractFields";

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
              fontFamily: "var(--font-mono)",
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
            slotProps={{
              input: {
                endAdornment: fieldHelpAdornment(
                  "The full party ID of a single Canton participant (e.g. alice::1220...). Paste the exact ID; it cannot be edited after the contract is created.",
                  "Help for Participant Party",
                ),
              },
            }}
          />
        );

      case "party_set":
        return (
          <Box
            sx={{ display: "flex", flexDirection: "column", gap: 1, flex: 1 }}
          >
            <TextField
              size="small"
              placeholder="Paste party ID, press Enter or click away"
              fullWidth
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  commitPartyInput(
                    e.currentTarget as HTMLInputElement,
                    field.parties,
                    (parties) => onChange({ ...field, parties }),
                  );
                  e.preventDefault();
                }
              }}
              onBlur={(e) =>
                commitPartyInput(e.target, field.parties, (parties) =>
                  onChange({ ...field, parties }),
                )
              }
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "The set of member parties that participate in governance. Paste each full party ID and press Enter to add. You can grow or shrink the set later via governance actions.",
                    "Help for Member Set",
                  ),
                },
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
                      sx={{ flex: 1, fontFamily: "var(--font-mono)" }}
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
            slotProps={{
              input: {
                endAdornment: fieldHelpAdornment(
                  "Minimum number of members that must approve a governance action before it executes. Defaults to a two-thirds majority.",
                  "Help for Governance Threshold",
                ),
              },
            }}
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
                slotProps={{
                  input: {
                    endAdornment: fieldHelpAdornment(
                      "Optional governance threshold override. Minimum approvals required for an action; leave at default for a two-thirds majority.",
                      "Help for Optional Governance Threshold",
                    ),
                  },
                }}
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
                slotProps={{
                  input: {
                    endAdornment: fieldHelpAdornment(
                      "Optional text value for this contract field. Free-form string passed to the contract as-is.",
                      "Help for Optional Text",
                    ),
                  },
                }}
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
                slotProps={{
                  input: {
                    endAdornment: fieldHelpAdornment(
                      "Optional integer value for this contract field. Whole number passed to the contract as-is.",
                      "Help for Optional Integer",
                    ),
                  },
                }}
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
                  placeholder={
                    label === "Disclosure Observers"
                      ? "Paste party ID, press Enter or click away (leave empty for no observers)"
                      : "Paste party ID, press Enter or click away (leave empty for no extra proposers)"
                  }
                  fullWidth
                  slotProps={{
                    input: {
                      endAdornment:
                        label === "Disclosure Observers"
                          ? fieldHelpAdornment(
                              "Parties that can see the rules contract without hosting the governance party. They cannot confirm or execute actions. Leave empty for none.",
                              "Help for Disclosure Observers",
                            )
                          : fieldHelpAdornment(
                              "Additional parties allowed to propose governance actions, beyond the regular members. Leave empty to keep the proposer allowlist at its default.",
                              "Help for Additional Proposers",
                            ),
                    },
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      const inner = field.inner as { parties: string[] };
                      commitPartyInput(
                        e.currentTarget as HTMLInputElement,
                        inner.parties,
                        (parties) =>
                          onChange({
                            ...field,
                            inner: { type: "party_set", parties },
                          }),
                      );
                      e.preventDefault();
                    }
                  }}
                  onBlur={(e) => {
                    const inner = field.inner as { parties: string[] };
                    commitPartyInput(e.target, inner.parties, (parties) =>
                      onChange({
                        ...field,
                        inner: { type: "party_set", parties },
                      }),
                    );
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
                            sx={{ flex: 1, fontFamily: "var(--font-mono)" }}
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
            slotProps={{
              input: {
                endAdornment: fieldHelpAdornment(
                  "Short identifier for the asset this contract operates on (e.g. CBTC). Must match the instrument symbol used by the deployed package.",
                  "Help for Instrument ID",
                ),
              },
            }}
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
            slotProps={{
              input: {
                endAdornment: fieldHelpAdornment(
                  "Free-form text value passed to the contract as-is.",
                  "Help for Text Value",
                ),
              },
            }}
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
            slotProps={{
              input: {
                endAdornment: fieldHelpAdornment(
                  "Whole-number value (signed 64-bit integer) passed to the contract as-is.",
                  "Help for Integer Value",
                ),
              },
            }}
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
          {(() => {
            const labelText =
              label ??
              FIELD_TYPES.find((t) => t.value === field.type)?.label ??
              field.type;
            const help = FIELD_TYPE_HELP[field.type];
            return help ? (
              <TextHelp text={help}>{labelText}</TextHelp>
            ) : (
              labelText
            );
          })()}
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

export { FieldEditor };
