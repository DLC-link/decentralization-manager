// Typed wrappers around the Rust in `decman-dapp`, compiled to WebAssembly.
// Every party id, topology transaction, hash, signature check and Ledger API
// body in the app comes from here, so it matches what the server and Canton
// compute.
import init, {
  allocateRefusal as allocateRefusalJson,
  allocationRequests as allocationRequestsJson,
  generateTopologyRequest as generateTopologyRequestJson,
  onboardingTransactions as onboardingTransactionsJson,
  partyHostedLocally as partyHostedLocallyJson,
  payloadDigest,
  planParty as planPartyJson,
  publicKeyNamespace,
  topologyVersionOfGenerated as topologyVersionOfGeneratedJson,
  verifySignature,
} from './wasm/decman_dapp'

export { payloadDigest, publicKeyNamespace }

export async function loadChecks(): Promise<void> {
  await init()
}

export type ProposedOwner = { publicKey: string; participant: string }

export type PartyProposal = {
  partyHint: string
  owners: ProposedOwner[]
  signingThreshold: number
  namespaceThreshold: number
  confirmationThreshold: number
  covalidators: string[]
}

export type PlannedOwner = { namespace: string; publicKey: string; participant: string }

export type PartyPlan = {
  partyId: string
  namespace: string
  owners: PlannedOwner[]
  hosts: string[]
  signingThreshold: number
  namespaceThreshold: number
  confirmationThreshold: number
}

export type OnboardingTransaction = { mapping: string; hash: string; transaction: string }

export type Onboarding = { transactions: OnboardingTransaction[]; combinedHash: string }

export type OwnerSignature = { publicKey: string; signature: string }

/** The two `POST /v2/parties/external/allocate` bodies. */
export type AllocationRequests = { hostingApproval: unknown; allocate: unknown }

/** Why a participant refused an allocate body. */
export type AllocateRefusal = 'partyExists' | 'namespaceConflict' | 'other'

/** Checks the proposal. Throws an Error with a message for the form. */
export function planParty(proposal: PartyProposal): PartyPlan {
  return JSON.parse(planPartyJson(JSON.stringify(proposal))) as PartyPlan
}

export function tryPlanParty(proposal: PartyProposal): { plan: PartyPlan } | { error: string } {
  try {
    return { plan: planParty(proposal) }
  } catch (error) {
    return { error: error instanceof Error ? error.message : String(error) }
  }
}

/** The transactions that create the party, in topology format 30 or 31. */
export function onboardingTransactions(proposal: PartyProposal, topologyVersion: number): Onboarding {
  return JSON.parse(onboardingTransactionsJson(JSON.stringify(proposal), topologyVersion)) as Onboarding
}

/** Throws unless every owner signed the combined hash. */
export function allocationRequests(
  proposal: PartyProposal,
  topologyVersion: number,
  synchronizerId: string,
  signatures: OwnerSignature[],
): AllocationRequests {
  return JSON.parse(
    allocationRequestsJson(JSON.stringify(proposal), topologyVersion, synchronizerId, JSON.stringify(signatures)),
  ) as AllocationRequests
}

export function generateTopologyRequest(synchronizerId: string, publicKey: string): unknown {
  return JSON.parse(generateTopologyRequestJson(synchronizerId, publicKey))
}

/** The topology format, 30 or 31, of a generate-topology answer. */
export function topologyVersionOfGenerated(response: unknown): number {
  return topologyVersionOfGeneratedJson(JSON.stringify(response))
}

export function allocateRefusal(answer: unknown, partyId: string): AllocateRefusal {
  return allocateRefusalJson(JSON.stringify(answer ?? null), partyId) as AllocateRefusal
}

export function partyHostedLocally(response: unknown, partyId: string): boolean {
  return partyHostedLocallyJson(JSON.stringify(response), partyId)
}

/** Malformed input counts as an invalid signature. */
export function isValidSignature(publicKey: string, digest: string, signature: string): boolean {
  try {
    return verifySignature(publicKey, digest, signature)
  } catch {
    return false
  }
}
