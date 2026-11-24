// Typed wrappers around the Rust checks in `decman-dapp`, compiled to
// WebAssembly. Every party id, digest and signature check in the app comes
// from here, so it matches what the server computes.
import init, {
  payloadDigest,
  planParty as planPartyJson,
  publicKeyNamespace,
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
  confirmationThreshold: number
  digest: string
}

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

/** Malformed input counts as an invalid signature. */
export function isValidSignature(publicKey: string, digest: string, signature: string): boolean {
  try {
    return verifySignature(publicKey, digest, signature)
  } catch {
    return false
  }
}
