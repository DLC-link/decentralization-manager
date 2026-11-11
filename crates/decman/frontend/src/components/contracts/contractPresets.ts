import type { ContractDefinition, PackageConfig } from "../../types";

type ContractType = "governance-core" | "cbtc" | null;

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
        // disclosureObservers : Optional (Set Party). Submitted as None while
        // empty; see `withEmptyObserversAsNone`.
        { type: "optional", inner: { type: "party_set", parties: [] } },
      ],
      fieldLabels: [
        "Dec. Party",
        "Member Set",
        "Threshold",
        "Proposal Timeout",
        "Additional Proposers",
        "Disclosure Observers",
      ],
    },
  ];
};

// An empty disclosure-observer list goes out as None. A node that has only
// governance-core 0.1.0 drops a trailing None field, but refuses a Some.
const withEmptyObserversAsNone = (
  contract: ContractDefinition,
): ContractDefinition => {
  if (contract.id !== "create-governance-rules") return contract;
  const observersIndex = contract.fields.length - 1;
  const observers = contract.fields[observersIndex];
  if (
    observers?.type !== "optional" ||
    observers.inner.type !== "party_set" ||
    observers.inner.parties.length > 0
  ) {
    return contract;
  }
  const fields = [...contract.fields];
  fields[observersIndex] = { type: "none" };
  return { ...contract, fields };
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
    default:
      return [];
  }
};

export { type ContractType, getContractsForType, withEmptyObserversAsNone };
