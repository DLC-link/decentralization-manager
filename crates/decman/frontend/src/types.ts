// Wire types for the decman frontend.
//
// Every wire type is generated from the Rust DTOs (via ts-rs) into
// `./types.generated.ts` and re-exported here, so a backend change shows up in
// the frontend type-check automatically. The generated file is gitignored and
// produced by `just gen-types` (and a pre-build step in CI / Docker / release).
//
// This file adds only what the backend does NOT generate: one UI-only field and
// a few aliases that bridge the names components import to the names the
// generator emits. A local export legitimately shadows the `export *`
// re-export, so the shadowing types below win for component code.

export * from "./types.generated";

import type {
  AuthConfigResponse,
  ContractDefinition as GeneratedContractDefinition,
  DisclosedContractInput,
  HoldingInfo,
  NodeConfigResponse,
  WorkflowProgress,
  WorkflowStatusResponse,
} from "./types.generated";

// The wire `ContractDefinition` plus a UI-only `fieldLabels` the contract
// builder uses to label fields; the backend ignores it (not generated).
export interface ContractDefinition extends GeneratedContractDefinition {
  /** Optional UI-only labels for each field, by index. Backend ignores. */
  fieldLabels?: string[];
}

// Name-bridge aliases: components import these names; the generator emits the
// right-hand names (the Rust type names).
export type AuthConfig = AuthConfigResponse;
export type DisclosedContract = DisclosedContractInput;
export type Holding = HoldingInfo;
export type NodeConfig = NodeConfigResponse;
export type KickStatus = WorkflowProgress;
export type OnboardingStatus = WorkflowProgress;
export type KickStatusResponse = WorkflowStatusResponse;
export type OnboardingStatusResponse = WorkflowStatusResponse;
export type ContractsStatusResponse = WorkflowStatusResponse;
export type DarsStatusResponse = WorkflowStatusResponse;
