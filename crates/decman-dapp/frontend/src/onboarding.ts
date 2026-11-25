// Creating a party on real participants. The Rust code builds the topology
// transactions and the request bodies. This file talks to the participants
// and keeps each one's answer in the proposal.
import {
  allocateRefusal,
  allocationRequests,
  generateTopologyRequest,
  isValidSignature,
  onboardingTransactions,
  partyHostedLocally,
  planParty,
  publicKeyNamespace,
  topologyVersionOfGenerated,
  type Onboarding,
  type PartyPlan,
  type PartyProposal,
} from './checks'
import * as ledger from './ledger'
import { generateKeyPair, signDigest } from './mock/keys'
import { isSimulated } from './mock/participants'
import {
  getState,
  update,
  type Covalidator,
  type HostResult,
  type HostStep,
  type Party,
  type ParticipantLink,
  type Proposal,
  type Signature,
  type Wallet,
} from './mock/store'

const HOSTING_TIMEOUT_MS = 120_000
const HOSTING_POLL_MS = 2_000

// Canton refuses a namespace definition at serial 1 that differs from the one
// these owners already have. The cause is often another threshold, but the
// participant's own text says what Canton found.
const NAMESPACE_CONFLICT =
  "These owners' namespace already exists with a different definition, for example another namespace threshold."

export const NOT_CONFIRMED = 'Not confirmed as created by this proposal'

export const NOT_CONFIRMED_NOTE =
  "No owner's participant confirmed that it accepted this proposal's bundle. The party may have existed already, or a reply was lost to a timeout or a reload."

/** Shown on every party page, since the dApp reads no topology yet. */
export const TOPOLOGY_NOTE =
  "The thresholds and hosts shown come from this browser's proposal. The dApp cannot read the party's topology yet."

const now = () => new Date().toISOString()
const newId = () => crypto.randomUUID()
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms))
const message = (error: unknown) => (error instanceof Error ? error.message : String(error))

async function linkParticipant(apiUrl: string, token: string): Promise<ParticipantLink> {
  const connection = { apiUrl: apiUrl.trim(), token: token.trim(), simulated: isSimulated() }
  if (!connection.apiUrl) throw new Error('Enter the JSON Ledger API URL.')
  return { ...connection, participantId: await ledger.participantId(connection) }
}

// ---- Wallets and covalidators ----------------------------------------------

export async function createWallet(name: string, apiUrl: string, token: string): Promise<void> {
  if (!name.trim()) throw new Error('Enter a name.')
  const participant = await linkParticipant(apiUrl, token)
  const keys = await generateKeyPair()
  const wallet: Wallet = {
    id: newId(),
    name: name.trim(),
    participant,
    ...keys,
    namespace: publicKeyNamespace(keys.publicKey),
  }
  update((s) => ({
    ...s,
    wallets: [...s.wallets, wallet],
    actingWalletId: s.actingWalletId ?? wallet.id,
  }))
}

export async function addCovalidator(name: string, apiUrl: string, token: string): Promise<void> {
  if (!name.trim()) throw new Error('Enter a name.')
  const participant = await linkParticipant(apiUrl, token)
  const covalidator: Covalidator = { id: newId(), name: name.trim(), participant }
  update((s) => ({ ...s, covalidators: [...s.covalidators, covalidator] }))
}

const modeName = (simulated: boolean) => (simulated ? 'simulated' : 'real')

/**
 * The connection, name and role of the host with `participantId`, among the
 * records of one mode. A record of the other mode never answers for it.
 */
function hostOf(
  participantId: string,
  simulated: boolean,
): { link: ParticipantLink; name: string; role: HostResult['role'] } {
  const { wallets, covalidators } = getState()
  const owners = wallets.filter(
    (w) => w.participant.participantId === participantId && w.participant.simulated === simulated,
  )
  if (owners.length > 0) {
    return { link: owners[0].participant, name: owners.map((w) => w.name).join(', '), role: 'owner' }
  }
  const covalidator = covalidators.find(
    (c) => c.participant.participantId === participantId && c.participant.simulated === simulated,
  )
  if (covalidator) return { link: covalidator.participant, name: covalidator.name, role: 'covalidator' }
  throw new Error(`No ${modeName(simulated)} wallet or covalidator uses participant ${participantId}.`)
}

/** The parts of a plan that a party record keeps, in one comparable form. */
function planSummary(plan: Pick<PartyPlan, 'owners' | 'signingThreshold' | 'namespaceThreshold' | 'confirmationThreshold'> & { hosts: string[] }): string {
  return JSON.stringify([
    plan.owners.map((o) => o.publicKey),
    plan.hosts,
    plan.signingThreshold,
    plan.namespaceThreshold,
    plan.confirmationThreshold,
  ])
}

/**
 * Refuses a party id that this browser already holds, as a party or an open
 * proposal, with another plan. A party id names one party.
 */
function checkNoOtherPlan(plan: PartyPlan, simulated: boolean): void {
  const { parties, proposals } = getState()
  const wanted = planSummary(plan)
  const otherParty = parties.some(
    (p) =>
      p.partyId === plan.partyId &&
      p.simulated === simulated &&
      planSummary({ ...p, hosts: p.hosts.map((h) => h.participant) }) !== wanted,
  )
  const otherProposal = proposals.some(
    (p) =>
      p.plan.partyId === plan.partyId &&
      p.simulated === simulated &&
      (isOpen(p) || p.status === 'submitting') &&
      !refusedEverywhere(p) &&
      planSummary(p.plan) !== wanted,
  )
  if (otherParty || otherProposal) {
    throw new Error(
      'This browser already holds this party id with other owners, hosts or thresholds. A party id names one party.',
    )
  }
}

/** Refuses a proposal whose owners or covalidators mix modes. */
function checkOneMode(proposal: PartyProposal, simulated: boolean): void {
  const { wallets, covalidators } = getState()
  const ownersMatch = proposal.owners.every((o) =>
    wallets.some((w) => w.publicKey === o.publicKey && w.participant.simulated === simulated),
  )
  const covalidatorsMatch = proposal.covalidators.every((id) =>
    covalidators.some((c) => c.participant.participantId === id && c.participant.simulated === simulated),
  )
  if (!ownersMatch || !covalidatorsMatch) {
    throw new Error(
      `Every owner and covalidator must use ${modeName(simulated)} participants, like the proposer's. Remove the others.`,
    )
  }
}

// ---- Proposals -------------------------------------------------------------

/**
 * Checks the proposal, reads the synchronizer and its topology format from
 * the proposer's participant, builds the transactions the owners sign, and
 * signs them as the proposer.
 */
export async function createProposal(proposal: PartyProposal, creator: Wallet): Promise<void> {
  const plan = planParty(proposal)
  if (!plan.owners.some((o) => o.publicKey === creator.publicKey)) {
    throw new Error('Act as one of the owners to propose the party.')
  }
  const { simulated } = creator.participant
  checkOneMode(proposal, simulated)
  checkNoOtherPlan(plan, simulated)
  const hosts = plan.hosts.map((participant): HostResult => {
    const role = roleOf(plan, participant)
    const ownerNames = getState()
      .wallets.filter((w) => plan.owners.some((o) => o.publicKey === w.publicKey && o.participant === participant))
      .map((w) => w.name)
    const name = role === 'owner' ? ownerNames.join(', ') : hostOf(participant, simulated).name
    return { participant, name, role, step: 'waiting' }
  })

  const synchronizers = await ledger.connectedSynchronizers(creator.participant)
  if (synchronizers.length !== 1) {
    throw new Error(
      `${creator.name}'s participant is connected to ${synchronizers.length} synchronizers. The dApp needs exactly one.`,
    )
  }
  const { synchronizerId } = synchronizers[0]
  const generated = await ledger.generateTopology(
    creator.participant,
    generateTopologyRequest(synchronizerId, creator.publicKey),
  )
  const topologyVersion = topologyVersionOfGenerated(generated)

  const entry: Proposal = {
    id: newId(),
    simulated,
    createdAt: now(),
    createdBy: creator.name,
    proposerKey: creator.publicKey,
    proposal,
    plan,
    synchronizerId,
    topologyVersion,
    onboarding: onboardingTransactions(proposal, topologyVersion),
    signatures: [],
    status: 'collecting',
    hosts,
  }
  // Creating the proposal carries the proposer's signature, after the same
  // check as any other signature.
  const { onboarding } = recheckProposal(entry)
  const signature = await signDigest(creator.privateKeyJwk, onboarding.combinedHash)
  entry.signatures = [{ publicKey: creator.publicKey, signature, signedAt: now() }]
  update((s) => ({ ...s, proposals: [entry, ...s.proposals] }))
}

function findProposal(id: string): Proposal {
  const proposal = getState().proposals.find((p) => p.id === id)
  if (!proposal) throw new Error('The proposal no longer exists.')
  return proposal
}

function changeProposal(id: string, change: (proposal: Proposal) => Proposal): void {
  update((s) => ({ ...s, proposals: s.proposals.map((p) => (p.id === id ? change(p) : p)) }))
}

function setHost(
  id: string,
  participant: string,
  step: HostStep,
  detail?: string,
  refusedByParticipant?: boolean,
): void {
  changeProposal(id, (p) => ({
    ...p,
    hosts: p.hosts.map((h) => (h.participant === participant ? { ...h, step, detail, refusedByParticipant } : h)),
  }))
}

/**
 * The plan and transactions, computed again from the proposal itself. The
 * stored copies are only a hint: an honest client checks them against what
 * it computes, and refuses to sign if they differ.
 */
export function recheckProposal(proposal: Proposal): { plan: PartyPlan; onboarding: Onboarding } {
  const plan = planParty(proposal.proposal)
  const onboarding = onboardingTransactions(proposal.proposal, proposal.topologyVersion)
  const hosts = proposal.hosts.map((h) => ({ participant: h.participant, role: h.role }))
  const planned = plan.hosts.map((participant) => ({ participant, role: roleOf(plan, participant) }))
  if (
    JSON.stringify(plan) !== JSON.stringify(proposal.plan) ||
    JSON.stringify(onboarding) !== JSON.stringify(proposal.onboarding) ||
    JSON.stringify(hosts) !== JSON.stringify(planned)
  ) {
    throw new Error('The stored proposal does not match the transactions it describes. Do not sign it.')
  }
  return { plan, onboarding }
}

/** A host is an owner's participant or a covalidator. */
function roleOf(plan: PartyPlan, participant: string): HostResult['role'] {
  return plan.owners.some((o) => o.participant === participant) ? 'owner' : 'covalidator'
}

/** The owners whose signature on the combined hash verifies. */
export function verifiedOwners(plan: PartyPlan, onboarding: Onboarding, signatures: Signature[]): Set<string> {
  const owners = plan.owners.map((o) => o.publicKey)
  return new Set(
    signatures
      .filter((s) => owners.includes(s.publicKey))
      .filter((s) => isValidSignature(s.publicKey, onboarding.combinedHash, s.signature))
      .map((s) => s.publicKey),
  )
}

export async function signProposal(id: string, wallet: Wallet): Promise<void> {
  const proposal = findProposal(id)
  const { plan, onboarding } = recheckProposal(proposal)
  if (!plan.owners.some((o) => o.publicKey === wallet.publicKey)) {
    throw new Error(`${wallet.name} is not an owner of this party.`)
  }
  checkOpen(proposal)
  // A stored signature that fails to verify is replaced, not kept.
  if (verifiedOwners(plan, onboarding, proposal.signatures).has(wallet.publicKey)) return
  const signature = await signDigest(wallet.privateKeyJwk, onboarding.combinedHash)
  const entry: Signature = { publicKey: wallet.publicKey, signature, signedAt: now() }
  changeProposal(id, (p) => ({
    ...p,
    signatures: [...p.signatures.filter((s) => s.publicKey !== wallet.publicKey), entry],
  }))
}

/** Whether the proposal can still be signed, withdrawn or rejected. */
export function isOpen(proposal: Proposal): boolean {
  return proposal.status === 'collecting' || proposal.status === 'failed'
}

function checkOpen(proposal: Proposal): void {
  if (!isOpen(proposal)) throw new Error('The proposal is no longer open.')
}

/**
 * Whether no host has received anything for the proposal. After a
 * submission the signed bundle may already sit at participants, and ending
 * the proposal in this browser would not take it back.
 */
export function untouched(proposal: Proposal): boolean {
  return proposal.status === 'collecting' && proposal.hosts.every((h) => h.step === 'waiting')
}

function checkUntouched(proposal: Proposal): void {
  if (!untouched(proposal)) {
    throw new Error('The signed bundle may already have reached participants. The proposal can only be tried again.')
  }
}

/**
 * Whether every owner's participant answered the last attempt with its own
 * refusal and none ever accepted the bundle. Then no participant holds it,
 * unlike after a timeout or a network error.
 */
export function refusedEverywhere(proposal: Proposal): boolean {
  const owners = proposal.hosts.filter((h) => h.role === 'owner')
  return (
    proposal.status === 'failed' &&
    !proposal.bundleAccepted &&
    owners.length > 0 &&
    owners.every((h) => h.step === 'failed' && h.refusedByParticipant)
  )
}

/**
 * The proposer ends the proposal before every owner has signed it, or after
 * every owner's participant refused it.
 */
export function withdrawProposal(id: string, wallet: Wallet): void {
  const proposal = findProposal(id)
  const { plan, onboarding } = recheckProposal(proposal)
  if (proposal.proposerKey !== wallet.publicKey) throw new Error('Only the proposer can withdraw the proposal.')
  if (!refusedEverywhere(proposal)) {
    checkUntouched(proposal)
    if (verifiedOwners(plan, onboarding, proposal.signatures).size === plan.owners.length) {
      throw new Error('Every owner has signed, so the proposal can no longer be withdrawn.')
    }
  }
  changeProposal(id, (p) => ({ ...p, status: 'withdrawn', endedBy: wallet.name }))
}

/**
 * An owner who has not signed declines. Creation needs every owner, so one
 * rejection ends the proposal.
 */
export function rejectProposal(id: string, wallet: Wallet): void {
  const proposal = findProposal(id)
  const { plan, onboarding } = recheckProposal(proposal)
  if (!plan.owners.some((o) => o.publicKey === wallet.publicKey)) {
    throw new Error(`${wallet.name} is not an owner of this party.`)
  }
  checkUntouched(proposal)
  if (verifiedOwners(plan, onboarding, proposal.signatures).has(wallet.publicKey)) {
    throw new Error(`${wallet.name} has already signed.`)
  }
  changeProposal(id, (p) => ({ ...p, status: 'rejected', endedBy: wallet.name }))
}

/**
 * Creates the party. Every owner's participant receives every transaction
 * and the owners' signatures, which is its consent to host. Then each
 * covalidator approves the pending hosting. The party exists once every host
 * reports it as local. A retry skips the hosts that already report it.
 */
export async function submitProposal(id: string, wallet: Wallet): Promise<void> {
  const proposal = findProposal(id)
  const { plan } = recheckProposal(proposal)
  if (!plan.owners.some((o) => o.publicKey === wallet.publicKey)) {
    throw new Error(`${wallet.name} is not an owner, so it cannot start the creation.`)
  }
  checkOpen(proposal)
  if (wallet.participant.simulated !== proposal.simulated) {
    throw new Error(`This proposal uses ${modeName(proposal.simulated)} participants. Act as an owner of that mode.`)
  }
  checkOneMode(proposal.proposal, proposal.simulated)
  if (getState().parties.some((p) => p.partyId === plan.partyId && p.simulated === proposal.simulated)) {
    throw new Error('This party already exists.')
  }
  const requests = allocationRequests(
    proposal.proposal,
    proposal.topologyVersion,
    proposal.synchronizerId,
    proposal.signatures.map(({ publicKey, signature }) => ({ publicKey, signature })),
  )
  changeProposal(id, (p) => ({
    ...p,
    status: 'submitting',
    error: undefined,
    hosts: p.hosts.map((h) => ({ ...h, step: 'waiting', detail: undefined, refusedByParticipant: undefined })),
  }))

  try {
    await markHostedHosts(id, plan.partyId, 'Already hosts the party.')
    await sendToHosts(id, plan.partyId, 'owner', requests.allocate, 'submitted', 'Received every transaction and the owner signatures.')
    await sendToHosts(id, plan.partyId, 'covalidator', requests.hostingApproval, 'approved', 'Signed its hosting approval.')
    await waitUntilHosted(id, plan.partyId)
  } catch (error) {
    changeProposal(id, (p) => ({ ...p, status: 'failed', error: message(error) }))
    throw error
  }

  addParty(findProposal(id), plan, wallet)
}

/** Marks every host that reports the party as local. */
async function markHostedHosts(id: string, partyId: string, detail: string): Promise<void> {
  const proposal = findProposal(id)
  await Promise.all(
    proposal.hosts
      .filter((h) => h.step !== 'hosted')
      .map(async (h) => {
        try {
          const answer = await ledger.getParty(hostOf(h.participant, proposal.simulated).link, partyId)
          if (partyHostedLocally(answer, partyId)) setHost(id, h.participant, 'hosted', detail)
        } catch (error) {
          setHost(id, h.participant, h.step, `Still waiting: ${message(error)}`)
        }
      }),
  )
}

/**
 * Sends `body` to every host with `role` that does not report the party yet.
 * A participant that says the party already exists counts as a success.
 */
async function sendToHosts(
  id: string,
  partyId: string,
  role: HostResult['role'],
  body: unknown,
  step: HostStep,
  detail: string,
): Promise<void> {
  const proposal = findProposal(id)
  const targets = proposal.hosts.filter((h) => h.role === role && h.step !== 'hosted')
  const results = await Promise.allSettled(
    targets.map((h) => ledger.allocateExternalParty(hostOf(h.participant, proposal.simulated).link, body)),
  )
  let refused = false
  results.forEach((result, index) => {
    const host = targets[index]
    if (result.status === 'fulfilled') {
      setHost(id, host.participant, step, detail)
      if (role === 'owner') changeProposal(id, (p) => ({ ...p, bundleAccepted: true }))
      return
    }
    const answer = result.reason instanceof ledger.LedgerError ? result.reason.answer : undefined
    const refusal = allocateRefusal(answer, partyId)
    if (refusal === 'partyExists') {
      setHost(id, host.participant, step, 'Says the party already exists.')
      return
    }
    refused = true
    const said = message(result.reason)
    setHost(
      id,
      host.participant,
      'failed',
      // Only an owner's participant receives the namespace definition. A
      // covalidator's refusal is shown as the participant wrote it.
      role === 'owner' && refusal === 'namespaceConflict' ? `${NAMESPACE_CONFLICT}\nThe participant said: ${said}` : said,
      result.reason instanceof ledger.LedgerError,
    )
  })
  if (refused) {
    throw new Error(
      role === 'owner'
        ? "An owner's participant refused the party. See its answer below."
        : 'A covalidator refused its hosting approval. See its answer below.',
    )
  }
}

async function waitUntilHosted(id: string, partyId: string): Promise<void> {
  const deadline = Date.now() + HOSTING_TIMEOUT_MS
  for (;;) {
    await markHostedHosts(id, partyId, 'Hosts the party.')
    const pending = findProposal(id).hosts.filter((h) => h.step !== 'hosted')
    if (pending.length === 0) return
    if (Date.now() > deadline) {
      throw new Error(`Timed out waiting for ${pending.map((h) => h.name).join(', ')} to host the party.`)
    }
    await sleep(HOSTING_POLL_MS)
  }
}

/** Records the party from the plan computed from the proposal itself. */
function addParty(proposal: Proposal, plan: PartyPlan, wallet: Wallet): void {
  // Without an accepted bundle, every owner's participant said the party
  // exists or already reported it, or its reply was lost.
  const unconfirmed = !proposal.bundleAccepted
  const party: Party = {
    partyId: plan.partyId,
    namespace: plan.namespace,
    owners: plan.owners,
    simulated: proposal.simulated,
    unconfirmed,
    hosts: plan.hosts.map((participant) => {
      const role = roleOf(plan, participant)
      return {
        participant,
        role,
        approval: proposal.simulated
          ? 'Approved by a simulated participant.'
          : role === 'owner'
            ? 'Approved by its own participant.'
            : "Approved by the dApp. This stands in for the covalidator's DecMan approval.",
      }
    }),
    signingThreshold: plan.signingThreshold,
    namespaceThreshold: plan.namespaceThreshold,
    confirmationThreshold: plan.confirmationThreshold,
    createdAt: now(),
    holdings: {},
    connections: [],
    requests: [],
    history: [
      {
        at: now(),
        text: `${unconfirmed ? `${NOT_CONFIRMED_NOTE} ` : ''}Every host reports the party.${proposal.simulated ? ' The hosts are simulated.' : ''} ${wallet.name} started the creation.`,
      },
    ],
  }
  update((s) => ({
    ...s,
    proposals: s.proposals.map((p) => (p.id === proposal.id ? { ...p, status: 'submitted', unconfirmed } : p)),
    parties: [party, ...s.parties],
  }))
}
