import type { ContractDefinition, FieldDefinition } from "../../types";

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

/// Hover-on-label tooltip text for each contract field type. Used by the
/// FieldEditor's label cell (in lockStructure mode) to show context about
/// what each field controls without rendering a `(?)` icon.
const FIELD_TYPE_HELP: Record<string, string> = {
  rel_time:
    "How long a governance proposal stays open before it expires. After this window, the proposal can no longer collect signatures and must be re-submitted.",
  bool: "Boolean (true/false) value passed to the contract as-is.",
  optional:
    "Pick the value type wrapped by this Optional field. Selecting a type sets a default value; leaving the inner field empty represents None.",
};

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

const commitPartyInput = (
  input: HTMLInputElement | HTMLTextAreaElement,
  parties: string[],
  onCommit: (parties: string[]) => void,
) => {
  const party = input.value.trim();
  if (!party) return;
  if (!parties.includes(party)) onCommit([...parties, party]);
  input.value = "";
};

export { FIELD_TYPES, FIELD_TYPE_HELP, createDefaultField, createEmptyContract, commitPartyInput };
